//! The frame as a Matter Air Quality Sensor (device type 0x002C), built on rs-matter.
//!
//! Endpoint 1 serves the room's CO2 (Carbon Dioxide Concentration Measurement), Air Quality, Temperature Measurement and
//! Relative Humidity Measurement clusters from the newest [`sensor::Latest`] reading, plus Identify. Controllers
//! commission it over the existing network (IP only; Bluetooth is off on the Pi), with the QR code or manual code
//! from [`Pairing`].
//!
//! rs-matter is async; the node runs on one thread under `block_on`, and tells the main loop when pairing changes
//! through [`crate::app::Event::Paired`].
//!
//! The vendor and product IDs are the CSA's test values (0xFFF1 / 0x8001) with rs-matter's test attestation
//! certificates, so Apple Home asks you to confirm an "uncertified accessory" and Google Home needs the IDs registered.

use crate::app::Event;
use crate::buttonbus::ButtonBus;
use crate::config;
use crate::pairing::{Code, MatterCommand};
use crate::sensor::{AirQuality, Latest, Reading};
use crate::switchlogic::SwitchEvent;
use anyhow::{Context, Result};
use core::pin::pin;
use embassy_futures::select::{select, select4};
use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::decl::air_quality::{self, AirQualityEnum};
use rs_matter::dm::clusters::decl::carbon_dioxide_concentration_measurement as co2;
use rs_matter::dm::clusters::decl::globals::{MeasurementMediumEnum, MeasurementUnitEnum};
use rs_matter::dm::clusters::decl::relative_humidity_measurement as humidity;
use rs_matter::dm::clusters::decl::switch::{self, InitialPress, LongPress, LongRelease, MultiPressComplete, MultiPressOngoing, ShortRelease};
use rs_matter::dm::clusters::fixed_label::{self, FixedLabelEntry, FixedLabelHandler};
use rs_matter::dm::clusters::decl::temperature_measurement as temperature;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::identify::{self, IdentifyHandler};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_PID, TEST_VID};
use rs_matter::dm::devices::DEV_TYPE_GENERIC_SWITCH;
use rs_matter::dm::endpoints;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, Cluster, DataModel, DeviceType, Dataver, Endpoint, HandlerContext, Node, ReadContext, SemanticTag};
use rs_matter::error::Error;
use rs_matter::im::{EthInteractionModelState, InteractionModel};
use rs_matter::pairing::qr::{no_optional_data, CommFlowType, NoOptionalData, Qr, QrPayload};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::DirKvBlobStore;
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::tlv::Nullable;
use rs_matter::transport::exchange::MatterBuffers;
use rs_matter::transport::MATTER_SOCKET_BIND_ADDR;
use rs_matter::utils::select::Coalesce;
use rs_matter::{clusters, devices, root_endpoint, with, BasicCommData, Matter, MATTER_PORT};
use serde::{Deserialize, Serialize};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

mod mdns;

/// The sensor lives on endpoint 1 (endpoint 0 is the root node).
const ENDPOINT: u16 = 1;
/// Air Quality Sensor, device type revision 1.
const DEV_TYPE_AIR_QUALITY_SENSOR: DeviceType = DeviceType { dtype: 0x002C, drev: 1 };
/// The panel buttons A to D are endpoints 2 to 5 (Generic Switch), when `[matter] buttons` is on.
const BUTTON_EP0: u16 = 2;
/// The Switch cluster.
const SWITCH_CLUSTER: u32 = 0x003B;
/// How often an attribute is looked at for a change worth telling subscribers about.
const WATCH: Duration = Duration::from_secs(5);

// ---- pairing codes ----

/// The setup passcode and discriminator printed in the QR code and manual pairing code. They are made once, kept in
/// `identity.json` (readable by the service only) so the code on the panel stays valid across restarts, and made
/// again after a factory reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    pub passcode: u32,
    pub discriminator: u16,
}

/// The passcodes the Matter specification forbids: 0, 11111111 to 99999999, 12345678 and 87654321. Also anything over
/// 8 digits.
pub fn passcode_is_valid(p: u32) -> bool {
    const FORBIDDEN: [u32; 2] = [12_345_678, 87_654_321];
    (1..=99_999_998).contains(&p) && !FORBIDDEN.contains(&p) && !(p % 11_111_111 == 0)
}

impl Pairing {
    pub fn random() -> Pairing {
        use rand::Rng;
        let mut rng = rand::rng();
        let passcode = loop {
            let p = rng.random_range(1..=99_999_998u32);
            if passcode_is_valid(p) {
                break p;
            }
        };
        Pairing { passcode, discriminator: rng.random_range(0..4096u16) }
    }

    pub fn load_or_create(dir: &Path) -> Result<Pairing> {
        let file = dir.join("identity.json");
        if let Ok(text) = std::fs::read_to_string(&file) {
            match serde_json::from_str::<Pairing>(&text) {
                Ok(p) if passcode_is_valid(p.passcode) && p.discriminator < 4096 => return Ok(p),
                _ => log::warn!("{} is damaged; making a new pairing code", file.display()),
            }
        }
        let p = Pairing::random();
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        write_private(&file, serde_json::to_string(&p)?.as_bytes())?;
        Ok(p)
    }

    fn comm_data(&self) -> BasicCommData {
        BasicCommData {
            password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&self.passcode.to_le_bytes())),
            discriminator: self.discriminator,
        }
    }

    /// The text of the QR code (`MT:...`), the grouped manual pairing code (`XXXX-XXX-XXXX`), and the QR code's modules
    /// (row by row, `true` = dark) with its width, for drawing it on the panel.
    pub fn onboarding(&self, device: &config::Matter) -> Result<Code> {
        let det = device_details(device, "0");
        let payload: QrPayload<'_, NoOptionalData> = QrPayload::new_from_basic_info(DiscoveryCapabilities::IP, CommFlowType::Standard, self.comm_data(), &det, no_optional_data as _);
        let mut buf = [0u8; 1024];
        let (text, rest) = payload.as_str(&mut buf).map_err(|e| anyhow::anyhow!("QR text: {e:?}"))?;
        let text = text.to_string();
        let (tmp, out) = rest.split_at_mut(rest.len() / 2);
        let qr = Qr::compute(&text, tmp, out).map_err(|e| anyhow::anyhow!("QR code: {e:?}"))?;
        let size = qr.size() as usize;
        let modules = (0..size).flat_map(|y| (0..size).map(move |x| (x, y))).map(|(x, y)| qr.get_module(x as i32, y as i32)).collect();
        Ok(Code { qr_text: text, manual_code: self.comm_data().compute_pretty_pairing_code().to_string(), width: size, modules })
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    Ok(std::fs::write(path, data)?)
}

/// Where rs-matter keeps its fabrics and counters.
pub fn store_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("matter")
}

/// The vendor-facing facts about the device. `serial` is an app-specific hash, never the raw machine-id.
fn device_details<'a>(cfg: &'a config::Matter, serial: &'a str) -> BasicInfoConfig<'a> {
    BasicInfoConfig {
        vid: TEST_VID,
        pid: TEST_PID,
        hw_ver: 1,
        hw_ver_str: "1",
        sw_ver: 1,
        sw_ver_str: env!("CARGO_PKG_VERSION"),
        serial_no: serial,
        manufacturing_date: "20221004",
        product_name: "InkyGadget air sensor",
        vendor_name: "InkyGadget",
        device_name: &cfg.device_name,
        ..BasicInfoConfig::new()
    }
}

/// The mDNS host name: 12 hex digits derived from the machine-id, so two frames on one network differ.
#[cfg(not(target_os = "macos"))]
fn mdns_host_name() -> String {
    format!("{:012X}", u64::from_str_radix(&serial_number()[2..], 16).unwrap_or(0) & 0xFFFF_FFFF_FFFF)
}

/// A stable serial number: a hash of the machine-id (or the host name), so the real id is never exposed.
fn serial_number() -> String {
    use std::hash::{Hash, Hasher};
    let id = std::fs::read_to_string("/etc/machine-id").or_else(|_| std::fs::read_to_string("/etc/hostname")).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("inkyframe-serial", id.trim()).hash(&mut h);
    format!("IG{:014X}", h.finish())
}

// ---- the sensor clusters ----

/// What the clusters read from.
#[derive(Clone)]
struct Room {
    latest: Latest,
    max_age: Duration,
    thresholds: [u16; 5],
}

impl Room {
    fn reading(&self) -> Option<Reading> {
        self.latest.get(self.max_age)
    }

    fn air_quality(&self) -> AirQuality {
        self.reading().map_or(AirQuality::Unknown, |r| AirQuality::from_co2(r.co2_ppm, &self.thresholds))
    }
}

/// Tell subscribers when `sample` changes (it should be coarse: a bucket of the value, not the value).
async fn watch<T: PartialEq>(ctx: impl HandlerContext, cluster: u32, mut sample: impl FnMut() -> T) -> Result<(), Error> {
    let mut last = sample();
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_millis(WATCH.as_millis() as u64)).await;
        let now = sample();
        if now != last {
            last = now;
            ctx.notify_cluster_changed(ENDPOINT, cluster);
        }
    }
}

// Quantised values: the attribute is exact, a report is sent when this changes.
fn temp_centi(r: &Reading) -> i16 {
    (r.temperature_c * 100.0).round().clamp(-4000.0, 12500.0) as i16
}
fn humidity_centi(r: &Reading) -> u16 {
    (r.humidity_pct * 100.0).round().clamp(0.0, 10000.0) as u16
}

struct AirQualityH {
    dataver: Dataver,
    room: Room,
}

impl air_quality::ClusterHandler for AirQualityH {
    const CLUSTER: Cluster<'static> = air_quality::FULL_CLUSTER
        .with_features((air_quality::Feature::FAIR.bits()) | air_quality::Feature::MODERATE.bits() | air_quality::Feature::VERY_POOR.bits() | air_quality::Feature::EXTREMELY_POOR.bits())
        .with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn air_quality(&self, _ctx: impl ReadContext) -> Result<AirQualityEnum, Error> {
        Ok(match self.room.air_quality() {
            AirQuality::Unknown => AirQualityEnum::Unknown,
            AirQuality::Good => AirQualityEnum::Good,
            AirQuality::Fair => AirQualityEnum::Fair,
            AirQuality::Moderate => AirQualityEnum::Moderate,
            AirQuality::Poor => AirQualityEnum::Poor,
            AirQuality::VeryPoor => AirQualityEnum::VeryPoor,
            AirQuality::ExtremelyPoor => AirQualityEnum::ExtremelyPoor,
        })
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        watch(ctx, Self::CLUSTER.id, || self.room.air_quality()).await
    }
}

struct Co2H {
    dataver: Dataver,
    room: Room,
}

impl co2::ClusterHandler for Co2H {
    // `required` alone leaves out the attributes the NumericMeasurement feature adds, so name them: without
    // MeasuredValue no controller shows a CO2 reading.
    const CLUSTER: Cluster<'static> = co2::FULL_CLUSTER.with_features(co2::Feature::NUMERIC_MEASUREMENT.bits()).with_attrs(with!(
        required;
        co2::AttributeId::MeasuredValue | co2::AttributeId::MinMeasuredValue | co2::AttributeId::MaxMeasuredValue | co2::AttributeId::MeasurementUnit
    ));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<f32>, Error> {
        Ok(Nullable::new(self.room.reading().map(|r| f32::from(r.co2_ppm))))
    }

    // The SCD40 measures 400 to 2000 ppm to specification and up to 40000 ppm.
    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<f32>, Error> {
        Ok(Nullable::some(0.0))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<f32>, Error> {
        Ok(Nullable::some(40_000.0))
    }

    fn measurement_unit(&self, _ctx: impl ReadContext) -> Result<MeasurementUnitEnum, Error> {
        Ok(MeasurementUnitEnum::PPM)
    }

    fn measurement_medium(&self, _ctx: impl ReadContext) -> Result<MeasurementMediumEnum, Error> {
        Ok(MeasurementMediumEnum::Air)
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        watch(ctx, Self::CLUSTER.id, || self.room.reading().map(|r| r.co2_ppm / 25)).await
    }
}

struct TemperatureH {
    dataver: Dataver,
    room: Room,
}

impl temperature::ClusterHandler for TemperatureH {
    const CLUSTER: Cluster<'static> = temperature::FULL_CLUSTER.with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::new(self.room.reading().map(|r| temp_centi(&r))))
    }

    // The SCD40 measures -10 to 60 C.
    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(-1000))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(6000))
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        watch(ctx, Self::CLUSTER.id, || self.room.reading().map(|r| temp_centi(&r) / 10)).await
    }
}

struct HumidityH {
    dataver: Dataver,
    room: Room,
}

impl humidity::ClusterHandler for HumidityH {
    const CLUSTER: Cluster<'static> = humidity::FULL_CLUSTER.with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::new(self.room.reading().map(|r| humidity_centi(&r))))
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(0))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(10_000))
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        watch(ctx, Self::CLUSTER.id, || self.room.reading().map(|r| humidity_centi(&r) / 100)).await
    }
}

/// One panel button as a Matter push button (Generic Switch with the momentary, release, long-press and multi-press
/// features). The button thread pushes events into the [`ButtonBus`]; this handler turns them into Switch events.
struct SwitchH {
    dataver: Dataver,
    /// 0 for button A to 3 for D.
    idx: usize,
    bus: Arc<ButtonBus>,
}

impl switch::ClusterHandler for SwitchH {
    // MultiPressMax is optional, so `required` alone would leave it out, and Home Assistant needs it to build its
    // double-press events (the same trap as the CO2 cluster).
    const CLUSTER: Cluster<'static> = switch::FULL_CLUSTER
        .with_features(
            switch::Feature::MOMENTARY_SWITCH.bits()
                | switch::Feature::MOMENTARY_SWITCH_RELEASE.bits()
                | switch::Feature::MOMENTARY_SWITCH_LONG_PRESS.bits()
                | switch::Feature::MOMENTARY_SWITCH_MULTI_PRESS.bits(),
        )
        .with_attrs(with!(required; switch::AttributeId::MultiPressMax));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    /// Up (0) and held down (1).
    fn number_of_positions(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(2)
    }

    fn current_position(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(u8::from(self.bus.is_down(self.idx)))
    }

    /// Taps counted in one sequence: a single tap or a double tap.
    fn multi_press_max(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(2)
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        let endpoint = BUTTON_EP0 + self.idx as u16;
        let mut held = self.bus.is_down(self.idx);
        loop {
            let event = self.bus.recv(self.idx).await;
            // Events go out at once; a full event queue loses this one, not the node.
            let sent = match event {
                SwitchEvent::InitialPress => InitialPress::emit_for(&ctx, endpoint, |b| b.new_position(1)?.end()).map(drop),
                SwitchEvent::ShortRelease => ShortRelease::emit_for(&ctx, endpoint, |b| b.previous_position(1)?.end()).map(drop),
                SwitchEvent::LongPress => LongPress::emit_for(&ctx, endpoint, |b| b.new_position(1)?.end()).map(drop),
                SwitchEvent::LongRelease => LongRelease::emit_for(&ctx, endpoint, |b| b.previous_position(1)?.end()).map(drop),
                SwitchEvent::MultiPressOngoing(n) => MultiPressOngoing::emit_for(&ctx, endpoint, |b| b.new_position(1)?.current_number_of_presses_counted(n)?.end()).map(drop),
                SwitchEvent::MultiPressComplete(n) => MultiPressComplete::emit_for(&ctx, endpoint, |b| b.previous_position(1)?.total_number_of_presses_counted(n)?.end()).map(drop),
            };
            if let Err(e) = sent {
                log::warn!("button {} event {event:?} not sent: {e:?}", self.idx);
            }
            let down = self.bus.is_down(self.idx);
            if down != held {
                held = down;
                ctx.notify_attr_changed(endpoint, SWITCH_CLUSTER, switch::AttributeId::CurrentPosition as u32);
            }
        }
    }
}

/// One panel button's endpoint: Generic Switch with Identify, a name, and the Number tag the spec asks for when a node
/// has several switches (position 1 to 4, top to bottom).
macro_rules! button_endpoint {
    ($i:literal) => {
        Endpoint::new(
            BUTTON_EP0 + $i,
            devices!(DEV_TYPE_GENERIC_SWITCH),
            clusters!(
                desc::CLUSTER_TAG_LIST,
                identify::CLUSTER,
                fixed_label::CLUSTER,
                <SwitchH as switch::ClusterHandler>::CLUSTER
            ),
        )
        .with_tags(&[SemanticTag::new(0x07, $i + 1)])
    };
}

/// The node's description: the root endpoint (Ethernet-style network commissioning stub, since the device is
/// commissioned over the network it is already on) and the sensor endpoint.
const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            ENDPOINT,
            devices!(DEV_TYPE_AIR_QUALITY_SENSOR),
            clusters!(
                desc::DescHandler::CLUSTER,
                identify::CLUSTER,
                <AirQualityH as air_quality::ClusterHandler>::CLUSTER,
                <Co2H as co2::ClusterHandler>::CLUSTER,
                <TemperatureH as temperature::ClusterHandler>::CLUSTER,
                <HumidityH as humidity::ClusterHandler>::CLUSTER
            ),
        ),
    ],
};

/// The same, plus the four panel buttons (endpoints 2 to 5).
const NODE_WITH_BUTTONS: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            ENDPOINT,
            devices!(DEV_TYPE_AIR_QUALITY_SENSOR),
            clusters!(
                desc::DescHandler::CLUSTER,
                identify::CLUSTER,
                <AirQualityH as air_quality::ClusterHandler>::CLUSTER,
                <Co2H as co2::ClusterHandler>::CLUSTER,
                <TemperatureH as temperature::ClusterHandler>::CLUSTER,
                <HumidityH as humidity::ClusterHandler>::CLUSTER
            ),
        ),
        button_endpoint!(0),
        button_endpoint!(1),
        button_endpoint!(2),
        button_endpoint!(3),
    ],
};

/// What the button endpoints need: the event queue, and the names shown for them.
struct Buttons<'a> {
    bus: Arc<ButtonBus>,
    labels: [[FixedLabelEntry<'a>; 1]; 4],
}

/// The handlers of one button's endpoint (a macro, because each `chain` is a new type).
macro_rules! with_button {
    ($handlers:expr, $i:literal, $rand:ident, $buttons:ident) => {
        $handlers
            .chain(|e, c| e == BUTTON_EP0 + $i && c == desc::DescHandler::CLUSTER.id, Async(desc::DescHandler::new(Dataver::new_rand(&mut $rand)).adapt()))
            .chain(|e, c| e == BUTTON_EP0 + $i && c == identify::CLUSTER.id, Async(IdentifyHandler::new(Dataver::new_rand(&mut $rand)).adapt()))
            .chain(
                |e, c| e == BUTTON_EP0 + $i && c == fixed_label::CLUSTER.id,
                Async(FixedLabelHandler::new(Dataver::new_rand(&mut $rand), &$buttons.labels[$i]).adapt()),
            )
            .chain(
                |e, c| e == BUTTON_EP0 + $i && c == SWITCH_CLUSTER,
                Async(switch::HandlerAdaptor(SwitchH { dataver: Dataver::new_rand(&mut $rand), idx: $i, bus: $buttons.bus.clone() })),
            )
    };
}

fn data_model<'a>(mut rand: impl rand10::Rng + Copy, room: &Room, node: Node<'static>, buttons: &'a Buttons<'a>) -> impl DataModel + 'a {
    let (aq, c, t, h) = (
        AirQualityH { dataver: Dataver::new_rand(&mut rand), room: room.clone() },
        Co2H { dataver: Dataver::new_rand(&mut rand), room: room.clone() },
        TemperatureH { dataver: Dataver::new_rand(&mut rand), room: room.clone() },
        HumidityH { dataver: Dataver::new_rand(&mut rand), room: room.clone() },
    );
    let handlers = endpoints::EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(|e, c| e == ENDPOINT && c == desc::DescHandler::CLUSTER.id, Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()))
            .chain(|e, c| e == ENDPOINT && c == identify::CLUSTER.id, Async(IdentifyHandler::new(Dataver::new_rand(&mut rand)).adapt()))
            .chain(|e, c| e == ENDPOINT && c == <AirQualityH as air_quality::ClusterHandler>::CLUSTER.id, Async(air_quality::HandlerAdaptor(aq)))
            .chain(|e, c| e == ENDPOINT && c == <Co2H as co2::ClusterHandler>::CLUSTER.id, Async(co2::HandlerAdaptor(c)))
            .chain(|e, c| e == ENDPOINT && c == <TemperatureH as temperature::ClusterHandler>::CLUSTER.id, Async(temperature::HandlerAdaptor(t)))
            .chain(|e, c| e == ENDPOINT && c == <HumidityH as humidity::ClusterHandler>::CLUSTER.id, Async(humidity::HandlerAdaptor(h)));
    // The button endpoints' handlers are always there; they are only reached when `node` lists those endpoints.
    let handlers = with_button!(handlers, 0, rand, buttons);
    let handlers = with_button!(handlers, 1, rand, buttons);
    let handlers = with_button!(handlers, 2, rand, buttons);
    let handlers = with_button!(handlers, 3, rand, buttons);
    (node, handlers)
}

/// Did the layout of endpoints and clusters change since the node last ran? Controllers keep a picture of it, and the
/// node's `ConfigurationVersion` must then rise (rs-matter stores that number itself; this only remembers the layout).
/// A first run, with nothing to compare with, is not a change.
pub fn layout_changed(dir: &Path, signature: &str) -> bool {
    let file = dir.join("layout.json");
    let old = std::fs::read_to_string(&file).ok();
    if old.as_deref() == Some(signature) {
        return false;
    }
    let saved = std::fs::create_dir_all(dir).map_err(anyhow::Error::from).and_then(|()| write_private(&file, signature.as_bytes()));
    if let Err(e) = saved {
        log::warn!("cannot save {}: {e:#}", file.display());
    }
    old.is_some()
}

/// Run the node until it fails. Blocks; call it on its own thread. `events` gets [`Event::Paired`] whenever the
/// device gains its first controller or loses its last; `commands` are what the main loop asks of the node.
pub fn run(cfg: &config::Config, latest: Latest, events: Sender<Event>, commands: &Receiver<MatterCommand>, bus: Option<Arc<ButtonBus>>) -> Result<()> {
    let dir = store_dir(&cfg.photos.state_dir);
    let pairing = Pairing::load_or_create(&dir)?;
    let serial = serial_number();
    let with_buttons = cfg.matter.buttons && bus.is_some();
    let det = device_details(&cfg.matter, &serial);
    let layout_changed = layout_changed(&dir, if with_buttons { "sensor+buttons" } else { "sensor" });
    let room = Room { latest, max_age: crate::sensor::max_age(&cfg.sensor), thresholds: cfg.sensor.thresholds_ppm };

    let matter = Matter::new(&det, pairing.comm_data(), &TEST_DEV_ATT, MATTER_PORT);
    let store = DirKvBlobStore::new(dir);
    let buffers: MatterBuffers = MatterBuffers::new();
    let state: EthInteractionModelState = EthInteractionModelState::new(EthNetwork::new_default());
    let kv = matter.kv(store);
    matter.startup(&kv).map_err(|e| anyhow::anyhow!("Matter start-up: {e:?}"))?;

    let crypto = default_crypto(rand10::rng(), DAC_PRIVKEY);
    let rand = crypto.rand().map_err(|e| anyhow::anyhow!("random numbers: {e:?}"))?;

    let names: [String; 4] = std::array::from_fn(|i| {
        let given = cfg.matter.button_labels[i].trim();
        if given.is_empty() { format!("Button {}", ['A', 'B', 'C', 'D'][i]) } else { given.to_string() }
    });
    let buttons = Buttons {
        bus: bus.unwrap_or_default(),
        labels: std::array::from_fn(|i| [FixedLabelEntry { label: "name", value: names[i].as_str() }]),
    };
    let node = if with_buttons { NODE_WITH_BUTTONS } else { NODE };
    let im = InteractionModel::new(&matter, &crypto, &buffers, data_model(rand, &room, node, &buttons), &kv, &state);
    futures_lite::future::block_on(im.startup()).map_err(|e| anyhow::anyhow!("data model start-up: {e:?}"))?;
    if layout_changed {
        // The endpoints changed (the buttons were switched on or off): tell controllers to look at the node again.
        let version = im.bump_configuration_version().map_err(|e| anyhow::anyhow!("configuration version: {e:?}"))?;
        log::info!("the node's layout changed; configuration version is now {version}");
    }

    let responder = DefaultResponder::new(&im);
    let mut respond = pin!(responder.run::<4, 4>());
    let mut im_job = pin!(im.run());
    let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR).context("cannot open the Matter port (UDP 5540)")?;
    let mut mdns = pin!(mdns::run(&matter, &crypto));
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

    if !matter.has_fabrics() {
        log::info!("not paired yet: pairing code {}", pairing.onboarding(&cfg.matter)?.manual_code);
        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &()).map_err(|e| anyhow::anyhow!("commissioning window: {e:?}"))?;
    }

    // Tell the main loop when pairing changes.
    let mut paired_watch = pin!(async {
        let mut last = matter.has_fabrics();
        let _ = events.send(Event::Paired(last));
        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
            while let Ok(cmd) = commands.try_recv() {
                match cmd {
                    // Only while nobody is paired: a paired controller opens windows from its own app.
                    MatterCommand::OpenWindow if !matter.has_fabrics() => {
                        match matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &()) {
                            Ok(()) => log::info!("pairing window opened again for 15 minutes"),
                            Err(e) => log::info!("pairing window not reopened (already open?): {e:?}"),
                        }
                    }
                    MatterCommand::OpenWindow => {}
                }
            }
            let now = matter.has_fabrics();
            if now != last {
                last = now;
                log::info!("{}", if now { "paired with a controller" } else { "no controllers left" });
                let _ = events.send(Event::Paired(now));
            }
        }
    });

    let core = select4(&mut transport, &mut mdns, &mut respond, &mut im_job);
    let all = select(core.coalesce(), &mut paired_watch);
    futures_lite::future::block_on(all.coalesce()).map_err(|e| anyhow::anyhow!("Matter stopped: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_passcodes_are_rejected() {
        for bad in [0, 11_111_111, 22_222_222, 99_999_999, 12_345_678, 87_654_321, 100_000_000, u32::MAX] {
            assert!(!passcode_is_valid(bad), "{bad}");
        }
        for good in [1, 20_202_021, 99_999_998, 11_111_112] {
            assert!(passcode_is_valid(good), "{good}");
        }
    }

    #[test]
    fn random_pairing_codes_are_always_valid() {
        for _ in 0..2000 {
            let p = Pairing::random();
            assert!(passcode_is_valid(p.passcode) && p.discriminator < 4096, "{p:?}");
        }
    }

    #[test]
    fn the_pairing_code_is_made_once_and_kept_private() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("matter");
        let first = Pairing::load_or_create(&dir).unwrap();
        assert_eq!(Pairing::load_or_create(&dir).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("identity.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(dir.join("identity.json"), r#"{"passcode":12345678,"discriminator":1}"#).unwrap();
        let again = Pairing::load_or_create(&dir).unwrap();
        assert!(passcode_is_valid(again.passcode), "a forbidden stored passcode is replaced");
        std::fs::write(dir.join("identity.json"), "garbage").unwrap();
        assert!(Pairing::load_or_create(&dir).is_ok(), "a damaged file is replaced");
    }

    #[test]
    fn onboarding_gives_a_scannable_code() {
        let cfg = config::Matter::default();
        let o = Pairing { passcode: 20_202_021, discriminator: 3840 }.onboarding(&cfg).unwrap();
        assert!(o.qr_text.starts_with("MT:"), "{}", o.qr_text);
        let digits: Vec<_> = o.manual_code.split('-').map(str::len).collect();
        assert_eq!(digits, [4, 3, 4], "{}", o.manual_code);
        assert_eq!(o.modules.len(), o.width * o.width);
        assert!((21..=41).contains(&o.width), "{}", o.width);
        // The three finder patterns are dark in their corners.
        let at = |x: usize, y: usize| o.modules[y * o.width + x];
        assert!(at(0, 0) && at(o.width - 1, 0) && at(0, o.width - 1), "finder patterns");
    }

    /// The strongest check short of a phone: draw the whole pairing screen and read the QR code back out of it.
    #[test]
    fn the_pairing_screen_can_be_scanned() {
        use crate::overlay::{DISPLAY_H, DISPLAY_W};
        let cfg = config::Matter::default();
        for (passcode, discriminator) in [(20_202_021, 3840), (1, 0), (99_999_998, 4095), (35_791_394, 2048)] {
            let code = Pairing { passcode, discriminator }.onboarding(&cfg).unwrap();
            let frame = crate::pairing::render(&code);
            let mut img = rqrr::PreparedImage::prepare_from_greyscale(DISPLAY_W, DISPLAY_H, |x, y| if frame[y * DISPLAY_W + x] == crate::palette::BLACK { 0 } else { 255 });
            let grids = img.detect_grids();
            assert!(!grids.is_empty(), "no QR code found for {passcode}");
            let (_, text) = grids[0].decode().unwrap_or_else(|e| panic!("cannot decode {passcode}: {e:?}"));
            assert_eq!(text, code.qr_text, "passcode {passcode}");
        }
    }

    #[test]
    fn different_codes_give_different_qr_text() {
        let cfg = config::Matter::default();
        let a = Pairing { passcode: 20_202_021, discriminator: 3840 }.onboarding(&cfg).unwrap();
        let b = Pairing { passcode: 20_202_022, discriminator: 3840 }.onboarding(&cfg).unwrap();
        assert_ne!(a.qr_text, b.qr_text);
        assert_ne!(a.manual_code, b.manual_code);
    }

    #[test]
    fn the_room_reads_unknown_until_the_sensor_reports() {
        let room = Room { latest: Latest::default(), max_age: Duration::from_secs(60), thresholds: [800, 1000, 1500, 2000, 5000] };
        assert_eq!(room.air_quality(), AirQuality::Unknown);
        room.latest.set(Reading { co2_ppm: 1200, temperature_c: 21.5, humidity_pct: 45.0 });
        assert_eq!(room.air_quality(), AirQuality::Moderate);
        assert_eq!(temp_centi(&room.reading().unwrap()), 2150);
        assert_eq!(humidity_centi(&room.reading().unwrap()), 4500);
    }

    /// The attribute ids a cluster actually serves.
    fn served(c: &Cluster<'static>) -> Vec<u32> {
        c.attributes.iter().map(|a| a.id).collect()
    }

    #[test]
    fn every_cluster_serves_the_attributes_controllers_need() {
        // Found on the Pi: CO2 once served only MeasurementMedium, so Home Assistant showed no CO2 value.
        let has = |c: &Cluster<'static>, ids: &[u32]| ids.iter().all(|i| served(c).contains(i));
        assert!(has(&<Co2H as co2::ClusterHandler>::CLUSTER, &[0x0, 0x1, 0x2, 0x8, 0x9]), "CO2: {:?}", served(&<Co2H as co2::ClusterHandler>::CLUSTER));
        assert!(has(&<TemperatureH as temperature::ClusterHandler>::CLUSTER, &[0x0, 0x1, 0x2]));
        assert!(has(&<HumidityH as humidity::ClusterHandler>::CLUSTER, &[0x0, 0x1, 0x2]));
        assert!(has(&<AirQualityH as air_quality::ClusterHandler>::CLUSTER, &[0x0]));
    }

    #[test]
    fn the_switch_cluster_serves_what_home_assistant_needs() {
        // NumberOfPositions, CurrentPosition and MultiPressMax (optional: `required` alone drops it).
        let c = <SwitchH as switch::ClusterHandler>::CLUSTER;
        assert!([0x0, 0x1, 0x2].iter().all(|i| served(&c).contains(i)), "{:?}", served(&c));
    }

    #[test]
    fn the_button_endpoints_are_four_distinct_switches() {
        assert_eq!(NODE.endpoints.len(), 2);
        assert_eq!(NODE_WITH_BUTTONS.endpoints.len(), 6);
        let mut tags = Vec::new();
        for (i, ep) in NODE_WITH_BUTTONS.endpoints[2..].iter().enumerate() {
            assert_eq!(ep.id, BUTTON_EP0 + i as u16);
            assert!(ep.device_types.iter().any(|d| d.dtype == 0x000F), "Generic Switch");
            let ids: Vec<u32> = ep.clusters.iter().map(|c| c.id).collect();
            for needed in [0x001D, 0x0003, 0x0040, 0x003B] {
                assert!(ids.contains(&needed), "endpoint {} lacks cluster {needed:#x}", ep.id);
            }
            assert_eq!(ep.semantic_tags.len(), 1, "a TagList is required when device types repeat");
            tags.push((ep.semantic_tags[0].namespace_id, ep.semantic_tags[0].tag));
        }
        assert_eq!(tags, [(7, 1), (7, 2), (7, 3), (7, 4)], "Number.One to Number.Four, top to bottom");
    }

    #[test]
    fn a_changed_layout_is_noticed_once() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("matter");
        assert!(!layout_changed(&dir, "sensor"), "a first run has nothing to compare with");
        assert!(!layout_changed(&dir, "sensor"));
        assert!(layout_changed(&dir, "sensor+buttons"));
        assert!(!layout_changed(&dir, "sensor+buttons"));
        assert!(layout_changed(&dir, "sensor"), "switching back is a change too");
    }

    #[test]
    fn extreme_readings_stay_inside_the_attribute_range() {
        let r = Reading { co2_ppm: 0, temperature_c: -300.0, humidity_pct: 400.0 };
        assert_eq!(temp_centi(&r), -4000);
        assert_eq!(humidity_centi(&r), 10_000);
        let hot = Reading { temperature_c: 900.0, ..r };
        assert_eq!(temp_centi(&hot), 12_500);
    }

    #[test]
    fn the_serial_number_is_stable_and_hides_the_machine_id() {
        let s = serial_number();
        assert_eq!(s, serial_number());
        assert!(s.starts_with("IG") && s.len() == 18, "{s}");
    }
}
