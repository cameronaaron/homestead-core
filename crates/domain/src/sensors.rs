//! Sensors: devices publishing to MQTT whose values become readings, and
//! rules that turn a value past a line into a task ("Bed 4 may need
//! watering").
//!
//! A rule fires once when its condition becomes true, then stays quiet
//! until the value comes back past its clear level (hysteresis), so a
//! probe hovering around 20 % raises one task, not one per reading.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::readings::ReadingSubject;
use crate::{TaskPriority, ValidationError, non_blank};

string_enum!(RuleOp, "op" { Below => "below", Above => "above" });

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewSensor {
    /// The MQTT topic it publishes on: `zigbee2mqtt/bed4`.
    pub topic: String,
    /// The JSON key holding its value (`soil_moisture`, or a dotted path
    /// like `sensor.temp`); empty when the payload is a bare number.
    #[serde(default)]
    pub field: String,
    /// "Bed 4 probe".
    pub name: String,
    /// The reading kind its values are filed under: "soil moisture".
    pub kind: String,
    #[serde(default = "value")]
    pub value_name: String,
    #[serde(default)]
    pub unit: String,
    /// A place, an animal or a machine; not a planting, which ends.
    pub subject: ReadingSubject,
    /// At most one reading kept per this many seconds.
    #[serde(default = "quarter_hour")]
    pub sample_secs: i32,
}

fn value() -> String {
    "value".into()
}

fn quarter_hour() -> i32 {
    900
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl NewSensor {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let topic = self.topic.trim().to_owned();
        if topic.is_empty() || topic.len() > 200 || topic.contains(['+', '#']) {
            return Err(ValidationError::Malformed("topic (one exact topic, no + or # wildcards)"));
        }
        let field = self.field.trim().to_owned();
        if field.len() > 60 {
            return Err(ValidationError::Malformed("field (60 characters at most)"));
        }
        let kind = squash(&self.kind).to_lowercase();
        let name = squash(&non_blank("name", &self.name)?);
        let value_name = squash(&self.value_name);
        let unit = squash(&self.unit);
        if kind.is_empty() {
            return Err(ValidationError::Blank("reading kind"));
        }
        if [&kind, &name, &value_name].iter().any(|s| s.chars().count() > 60) || unit.chars().count() > 20 {
            return Err(ValidationError::Malformed("sensor (names 60 characters, units 20 at most)"));
        }
        if matches!(self.subject, ReadingSubject::Planting(_)) {
            return Err(ValidationError::Malformed("what it measures (a place, an animal or equipment)"));
        }
        if !(60..=86_400).contains(&self.sample_secs) {
            return Err(ValidationError::Malformed("sample interval (between a minute and a day)"));
        }
        Ok(Self {
            topic,
            field,
            name,
            kind,
            value_name: if value_name.is_empty() { value() } else { value_name },
            unit,
            ..self
        })
    }
}

/// The sensor's number out of an MQTT payload: a bare number when `field`
/// is empty, otherwise the (dotted) key of a JSON object, holding a number
/// or a numeric string.
pub fn extract(payload: &str, field: &str) -> Result<Decimal, ValidationError> {
    let bad = || ValidationError::Malformed("payload (no number where the sensor says its value is)");
    let number = |s: &str| s.trim().parse::<Decimal>().or_else(|_| Decimal::from_scientific(s.trim())).map_err(|_| bad());
    if field.is_empty() {
        return number(payload);
    }
    let json: serde_json::Value = serde_json::from_str(payload).map_err(|_| bad())?;
    let mut at = &json;
    for key in field.split('.') {
        at = at.get(key).ok_or_else(bad)?;
    }
    match at {
        serde_json::Value::Number(n) => number(&n.to_string()),
        serde_json::Value::String(s) => number(s),
        _ => Err(bad()),
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewRule {
    pub op: RuleOp,
    pub threshold: Decimal,
    /// Where the value must come back to before the rule can fire again;
    /// `None` is the threshold itself.
    #[serde(default)]
    pub clear_at: Option<Decimal>,
    /// The task it raises: "Bed 4 may need watering".
    pub task_title: String,
    #[serde(default = "high")]
    pub priority: TaskPriority,
}

fn high() -> TaskPriority {
    TaskPriority::High
}

impl NewRule {
    pub fn validated(self) -> Result<Self, ValidationError> {
        let task_title = squash(&non_blank("task title", &self.task_title)?);
        if task_title.chars().count() > 120 {
            return Err(ValidationError::Malformed("task title (120 characters at most)"));
        }
        if let Some(c) = self.clear_at {
            let wrong_side = match self.op {
                RuleOp::Below => c < self.threshold,
                RuleOp::Above => c > self.threshold,
            };
            if wrong_side {
                return Err(ValidationError::Malformed(
                    "clear level (it must be on the healthy side of the threshold: above it for “below”, below it for “above”)",
                ));
            }
        }
        Ok(Self { task_title, ..self })
    }
}

/// A rule as the bridge needs it: enough to tell, on the farm, that a
/// value just crossed a line and the batch should go now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Watch {
    pub topic: String,
    pub field: String,
    pub op: RuleOp,
    pub threshold: Decimal,
    pub clear_at: Decimal,
    pub armed: bool,
}

impl Watch {
    /// Whether `v` would fire this rule now.
    pub fn fires(&self, v: Decimal) -> bool {
        self.armed && past(self.op, self.threshold, v)
    }
}

fn past(op: RuleOp, threshold: Decimal, v: Decimal) -> bool {
    match op {
        RuleOp::Below => v < threshold,
        RuleOp::Above => v > threshold,
    }
}

/// What a new value does to a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Raise the task and disarm.
    Fire,
    /// Back to healthy: arm again.
    Rearm,
    Nothing,
}

pub fn step(op: RuleOp, threshold: Decimal, clear_at: Option<Decimal>, armed: bool, v: Decimal) -> Step {
    let clear = clear_at.unwrap_or(threshold);
    let healthy = match op {
        RuleOp::Below => v >= clear,
        RuleOp::Above => v <= clear,
    };
    if armed && past(op, threshold, v) {
        Step::Fire
    } else if !armed && healthy {
        Step::Rearm
    } else {
        Step::Nothing
    }
}

/// A sensor as the store returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sensor {
    pub id: Uuid,
    pub topic: String,
    pub field: String,
    pub name: String,
    pub kind: String,
    pub value_name: String,
    pub unit: String,
    pub subject: ReadingSubject,
    pub subject_name: String,
    pub sample_secs: i32,
    pub active: bool,
    pub last_value: Option<Decimal>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rule {
    pub id: Uuid,
    pub sensor_id: Uuid,
    pub op: RuleOp,
    pub threshold: Decimal,
    pub clear_at: Option<Decimal>,
    pub task_title: String,
    pub priority: TaskPriority,
    pub armed: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_fired_at: Option<time::OffsetDateTime>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().expect("decimal")
    }

    #[test]
    fn check_lists_match_migration() {
        let sql = include_str!("../../../migrations/0048_sensors.sql");
        crate::tests::assert_check_list(sql, "op", RuleOp::ALL, RuleOp::as_str);
    }

    #[test]
    fn values_come_out_of_the_payloads_devices_send() {
        assert_eq!(extract(" 18.5\n", ""), Ok(d("18.5")));
        let z2m = r#"{"soil_moisture":18,"temperature":21.37,"battery":"97","nested":{"t":1.5e1}}"#;
        assert_eq!(extract(z2m, "soil_moisture"), Ok(d("18")));
        assert_eq!(extract(z2m, "temperature"), Ok(d("21.37")));
        assert_eq!(extract(z2m, "battery"), Ok(d("97")), "a numeric string");
        assert_eq!(extract(z2m, "nested.t"), Ok(d("15")), "a dotted path, scientific notation");
        assert!(extract(z2m, "humidity").is_err(), "a missing key");
        assert!(extract(r#"{"state":"ON"}"#, "state").is_err());
        assert!(extract("ON", "").is_err());
        assert!(extract(z2m, "").is_err(), "JSON where a bare number was expected");
    }

    #[test]
    fn a_rule_fires_once_per_dry_spell() {
        // Soil moisture below 20 %, clear again at 25 %.
        let (t, c) = (d("20"), Some(d("25")));
        let mut armed = true;
        let mut fired = 0;
        for v in ["30", "22", "19", "18", "21", "19", "24", "26", "19"] {
            match step(RuleOp::Below, t, c, armed, d(v)) {
                Step::Fire => {
                    fired += 1;
                    armed = false;
                }
                Step::Rearm => armed = true,
                Step::Nothing => {}
            }
        }
        assert_eq!(fired, 2, "19 fires; hovering 18–24 doesn't; 26 re-arms; 19 fires again");
        assert_eq!(step(RuleOp::Above, d("100"), None, true, d("100")), Step::Nothing, "at the line isn't past it");
        assert_eq!(step(RuleOp::Above, d("100"), None, false, d("100")), Step::Rearm);
        let w = Watch { topic: "t".into(), field: String::new(), op: RuleOp::Above, threshold: d("95"), clear_at: d("90"), armed: true };
        assert!(w.fires(d("96")) && !w.fires(d("95")));
        assert!(!Watch { armed: false, ..w }.fires(d("120")), "a fired rule waits to re-arm");
    }

    #[test]
    fn a_clear_level_on_the_wrong_side_is_refused() {
        let rule = |op, clear: &str| NewRule {
            op,
            threshold: d("20"),
            clear_at: Some(d(clear)),
            task_title: " Bed 4 may need  watering ".into(),
            priority: TaskPriority::High,
        };
        assert_eq!(rule(RuleOp::Below, "25").validated().expect("ok").task_title, "Bed 4 may need watering");
        assert!(rule(RuleOp::Below, "15").validated().is_err());
        assert!(rule(RuleOp::Above, "25").validated().is_err());
        assert!(rule(RuleOp::Above, "15").validated().is_ok());
    }

    #[test]
    fn sensors_need_an_exact_topic_and_a_lasting_subject() {
        let s = NewSensor {
            topic: " zigbee2mqtt/bed4 ".into(),
            field: "soil_moisture".into(),
            name: "Bed 4 probe".into(),
            kind: "Soil Moisture".into(),
            value_name: String::new(),
            unit: "%".into(),
            subject: ReadingSubject::Location(Uuid::nil()),
            sample_secs: 900,
        };
        let ok = s.clone().validated().expect("ok");
        assert_eq!((ok.topic.as_str(), ok.kind.as_str(), ok.value_name.as_str()), ("zigbee2mqtt/bed4", "soil moisture", "value"));
        assert!(NewSensor { topic: "zigbee2mqtt/#".into(), ..s.clone() }.validated().is_err());
        assert!(NewSensor { subject: ReadingSubject::Planting(Uuid::nil()), ..s.clone() }.validated().is_err());
        assert!(NewSensor { sample_secs: 5, ..s }.validated().is_err());
    }
}
