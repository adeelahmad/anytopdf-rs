//! Reads a GPS fix from the flattened ExifTool / ffprobe source metadata.

use anytopdf_core::Metadata;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GpsFix {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    /// Metadata key the fix was read from.
    pub origin: String,
}

/// ExifTool groups in the order their coordinates are trusted. Composite tags
/// already fold in the hemisphere reference.
const GROUPS: &[&str] = &[
    "Composite",
    "GPS",
    "XMP-exif",
    "Keys",
    "UserData",
    "ItemList",
];

pub(crate) fn gps_fix(metadata: &Metadata) -> Option<GpsFix> {
    exiftool_pair(metadata)
        .or_else(|| exiftool_combined(metadata))
        .or_else(|| ffprobe_iso6709(metadata))
        .filter(|fix| plausible(fix.latitude, fix.longitude))
}

fn plausible(latitude: f64, longitude: f64) -> bool {
    // Cameras without a fix often write 0,0 ("null island").
    latitude.is_finite()
        && longitude.is_finite()
        && latitude.abs() <= 90.0
        && longitude.abs() <= 180.0
        && !(latitude == 0.0 && longitude == 0.0)
}

fn exiftool_keys<'a>(metadata: &'a Metadata, suffix: &str) -> Vec<(&'a String, &'a String)> {
    let mut keys: Vec<_> = metadata
        .iter()
        .filter(|(k, _)| k.starts_with("exiftool.") && k.ends_with(suffix))
        .collect();
    keys.sort_by_key(|(k, _)| group_rank(k));
    keys
}

fn group(key: &str) -> &str {
    key.strip_prefix("exiftool.")
        .and_then(|rest| rest.split(':').next())
        .unwrap_or("")
}

fn group_rank(key: &str) -> usize {
    let g = group(key);
    GROUPS.iter().position(|p| *p == g).unwrap_or(GROUPS.len())
}

fn exiftool_pair(metadata: &Metadata) -> Option<GpsFix> {
    for (lat_key, lat_value) in exiftool_keys(metadata, ":GPSLatitude") {
        let prefix = lat_key.strip_suffix("Latitude")?;
        let Some(lon_value) = metadata.get(&format!("{prefix}Longitude")) else {
            continue;
        };
        let lat_ref = metadata.get(&format!("{prefix}LatitudeRef"));
        let lon_ref = metadata.get(&format!("{prefix}LongitudeRef"));
        let (Some(mut latitude), Some(mut longitude)) =
            (parse_angle(lat_value), parse_angle(lon_value))
        else {
            continue;
        };
        if lat_ref.is_some_and(|r| negative_hemisphere(r)) {
            latitude = -latitude.abs();
        }
        if lon_ref.is_some_and(|r| negative_hemisphere(r)) {
            longitude = -longitude.abs();
        }
        let altitude = metadata
            .get(&format!("{prefix}Altitude"))
            .and_then(|a| {
                parse_altitude(
                    a,
                    metadata
                        .get(&format!("{prefix}AltitudeRef"))
                        .map(String::as_str),
                )
            })
            .or_else(|| {
                metadata
                    .get("exiftool.Composite:GPSAltitude")
                    .and_then(|a| parse_altitude(a, None))
            });
        return Some(GpsFix {
            latitude,
            longitude,
            altitude,
            origin: lat_key.clone(),
        });
    }
    None
}

/// `GPSPosition` / QuickTime `GPSCoordinates`: "lat, lon[, alt]".
fn exiftool_combined(metadata: &Metadata) -> Option<GpsFix> {
    let mut keys = exiftool_keys(metadata, ":GPSPosition");
    keys.extend(exiftool_keys(metadata, ":GPSCoordinates"));
    keys.into_iter().find_map(|(key, value)| {
        let mut parts = value.split(',');
        let latitude = parse_angle(parts.next()?)?;
        let longitude = parse_angle(parts.next()?)?;
        let altitude = parts.next().and_then(|a| parse_altitude(a, None));
        Some(GpsFix {
            latitude,
            longitude,
            altitude,
            origin: key.clone(),
        })
    })
}

fn ffprobe_iso6709(metadata: &Metadata) -> Option<GpsFix> {
    let mut keys: Vec<_> = metadata
        .iter()
        .filter(|(k, _)| {
            k.starts_with("ffprobe.")
                && (k.ends_with(".location")
                    || k.contains(".location-")
                    || k.ends_with("location.ISO6709"))
        })
        .collect();
    // Prefer container tags over per-stream tags.
    keys.sort_by_key(|(k, _)| !k.starts_with("ffprobe.format."));
    keys.into_iter().find_map(|(key, value)| {
        let (latitude, longitude, altitude) = parse_iso6709(value)?;
        Some(GpsFix {
            latitude,
            longitude,
            altitude,
            origin: key.clone(),
        })
    })
}

fn negative_hemisphere(text: &str) -> bool {
    let t = text.trim();
    t.eq_ignore_ascii_case("s")
        || t.eq_ignore_ascii_case("w")
        || t.eq_ignore_ascii_case("south")
        || t.eq_ignore_ascii_case("west")
}

fn numbers(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() || (c == '.' && !current.contains('.')) {
            current.push(c);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse() {
                out.push(n);
            }
            current.clear();
        }
    }
    out
}

/// Parses ExifTool angles: `37 deg 23' 10.98" N`, `37.386 N`, `-122.08`, `37,23.18N`.
pub(crate) fn parse_angle(text: &str) -> Option<f64> {
    let text = text.trim();
    let nums = numbers(text);
    if nums.is_empty() || nums.len() > 3 {
        return None;
    }
    let (minutes, seconds) = (
        nums.get(1).copied().unwrap_or(0.0),
        nums.get(2).copied().unwrap_or(0.0),
    );
    if minutes >= 60.0 || seconds >= 60.0 {
        return None;
    }
    let magnitude = nums[0] + minutes / 60.0 + seconds / 3600.0;
    let hemisphere = text
        .rsplit(|c: char| !c.is_ascii_alphabetic())
        .find(|w| !w.is_empty())
        .filter(|w| !w.eq_ignore_ascii_case("deg"));
    let negative = text.starts_with('-') || hemisphere.is_some_and(negative_hemisphere);
    Some(if negative { -magnitude } else { magnitude })
}

/// `12.3 m`, `12.3 m Above Sea Level`, `5 m Below Sea Level`.
fn parse_altitude(text: &str, reference: Option<&str>) -> Option<f64> {
    let value = *numbers(text).first()?;
    let below = text.contains("Below")
        || reference.is_some_and(|r| r.contains("Below") || r.trim() == "1")
        || text.trim_start().starts_with('-');
    Some(if below { -value } else { value })
}

/// Parses an ISO 6709 point such as `+37.3861-122.0838+012.300/` (QuickTime
/// `location` / `com.apple.quicktime.location.ISO6709`). Degrees may also be
/// written as ±DDMM.MMMM or ±DDMMSS.SS.
pub(crate) fn parse_iso6709(text: &str) -> Option<(f64, f64, Option<f64>)> {
    let body = text.trim().trim_end_matches('/');
    let body = body.split("CRS").next().unwrap_or(body);
    let mut fields = Vec::new();
    let mut start = None;
    for (i, c) in body.char_indices() {
        if c == '+' || c == '-' {
            if let Some(s) = start {
                fields.push(&body[s..i]);
            }
            start = Some(i);
        } else if !(c.is_ascii_digit() || c == '.') {
            return None;
        }
    }
    fields.push(&body[start?..]);
    if !(2..=3).contains(&fields.len()) {
        return None;
    }
    let latitude = iso_component(fields[0], 2)?;
    let longitude = iso_component(fields[1], 3)?;
    let altitude = fields.get(2).and_then(|a| a.parse::<f64>().ok());
    Some((latitude, longitude, altitude))
}

fn iso_component(field: &str, degree_digits: usize) -> Option<f64> {
    let (sign, digits) = field.split_at(1);
    let sign = if sign == "-" { -1.0 } else { 1.0 };
    let int_len = digits.find('.').unwrap_or(digits.len());
    let value: f64 = digits.parse().ok()?;
    let degrees = match int_len.checked_sub(degree_digits)? {
        0 => value,
        2 => {
            let d = (value / 100.0).trunc();
            let m = value - d * 100.0;
            (m < 60.0).then_some(d + m / 60.0)?
        }
        4 => {
            let d = (value / 10_000.0).trunc();
            let m = ((value - d * 10_000.0) / 100.0).trunc();
            let s = value - d * 10_000.0 - m * 100.0;
            (m < 60.0 && s < 60.0).then_some(d + m / 60.0 + s / 3600.0)?
        }
        _ => return None,
    };
    Some(sign * degrees)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, &str)]) -> Metadata {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn angle_parses_exiftool_degrees_minutes_seconds_with_hemisphere() {
        assert!(close(
            parse_angle("37 deg 23' 10.98\" N").unwrap(),
            37.386383
        ));
        assert!(close(
            parse_angle("122 deg 5' 1.86\" W").unwrap(),
            -122.083850
        ));
        assert!(close(
            parse_angle("33 deg 52' 0.00\" S").unwrap(),
            -33.866667
        ));
        assert!(close(parse_angle("-122.0838").unwrap(), -122.0838));
        assert!(close(parse_angle("51.5085 N").unwrap(), 51.5085));
        assert_eq!(parse_angle("north"), None);
        assert_eq!(parse_angle("10 deg 75' 0\" N"), None);
    }

    #[test]
    fn gps_fix_prefers_composite_tags() {
        let fix = gps_fix(&meta(&[
            ("exiftool.GPS:GPSLatitude", "37 deg 23' 10.98\""),
            ("exiftool.GPS:GPSLatitudeRef", "North"),
            ("exiftool.GPS:GPSLongitude", "122 deg 5' 1.86\""),
            ("exiftool.GPS:GPSLongitudeRef", "West"),
            ("exiftool.Composite:GPSLatitude", "48 deg 51' 24.00\" N"),
            ("exiftool.Composite:GPSLongitude", "2 deg 21' 8.00\" E"),
            ("exiftool.Composite:GPSAltitude", "35 m Above Sea Level"),
        ]))
        .unwrap();
        assert!(close(fix.latitude, 48.856667), "{fix:?}");
        assert!(close(fix.longitude, 2.352222), "{fix:?}");
        assert_eq!(fix.altitude, Some(35.0));
        assert_eq!(fix.origin, "exiftool.Composite:GPSLatitude");
    }

    #[test]
    fn gps_fix_applies_raw_gps_hemisphere_references() {
        let fix = gps_fix(&meta(&[
            ("exiftool.GPS:GPSLatitude", "33 deg 52' 0.00\""),
            ("exiftool.GPS:GPSLatitudeRef", "South"),
            ("exiftool.GPS:GPSLongitude", "151 deg 12' 30.00\""),
            ("exiftool.GPS:GPSLongitudeRef", "East"),
            ("exiftool.GPS:GPSAltitude", "4 m"),
            ("exiftool.GPS:GPSAltitudeRef", "Below Sea Level"),
        ]))
        .unwrap();
        assert!(close(fix.latitude, -33.866667), "{fix:?}");
        assert!(close(fix.longitude, 151.208333), "{fix:?}");
        assert_eq!(fix.altitude, Some(-4.0));
    }

    #[test]
    fn gps_fix_reads_quicktime_gps_coordinates() {
        let fix = gps_fix(&meta(&[(
            "exiftool.Keys:GPSCoordinates",
            "40 deg 26' 46.80\" N, 79 deg 58' 55.20\" W, 12.3 m Above Sea Level",
        )]))
        .unwrap();
        assert!(close(fix.latitude, 40.446333), "{fix:?}");
        assert!(close(fix.longitude, -79.982), "{fix:?}");
        assert_eq!(fix.altitude, Some(12.3));
    }

    #[test]
    fn gps_fix_reads_ffprobe_iso6709_tags() {
        let fix = gps_fix(&meta(&[(
            "ffprobe.format.tags.com.apple.quicktime.location.ISO6709",
            "+37.3861-122.0838+012.300/",
        )]))
        .unwrap();
        assert!(close(fix.latitude, 37.3861));
        assert!(close(fix.longitude, -122.0838));
        assert_eq!(fix.altitude, Some(12.3));
        assert!(
            gps_fix(&meta(&[(
                "ffprobe.format.tags.location",
                "+48.8566+002.3522/"
            )]))
            .is_some()
        );
    }

    #[test]
    fn gps_fix_ignores_null_island_and_missing_tags() {
        assert_eq!(gps_fix(&Metadata::new()), None);
        assert_eq!(
            gps_fix(&meta(&[(
                "ffprobe.format.tags.location",
                "+00.0000+000.0000/"
            )])),
            None
        );
        assert_eq!(
            gps_fix(&meta(&[("exiftool.Composite:GPSLatitude", "48 deg N")])),
            None
        );
    }

    #[test]
    fn iso6709_accepts_minute_and_second_forms() {
        let (lat, lon, alt) = parse_iso6709("+4030.5-07359.5/").unwrap();
        assert!(close(lat, 40.508333) && close(lon, -73.991667) && alt.is_none());
        let (lat, lon, _) = parse_iso6709("+403000-0735930CRSWGS_84/").unwrap();
        assert!(close(lat, 40.5) && close(lon, -73.991667));
        assert_eq!(parse_iso6709("+40.5"), None);
        assert_eq!(parse_iso6709("Paris"), None);
        assert_eq!(parse_iso6709("+406000-0735930/"), None);
    }
}
