use std::{fs::File, io::BufReader, path::Path};

use chrono::{Local, NaiveDateTime, TimeZone};
use exif::{Exif, In, Tag, Value};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct PhotoMetadata {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub software: Option<String>,
    pub exposure_time: Option<f64>,
    pub focal_length: Option<f64>,
    pub image_orientation: Option<String>,
    pub aperture: Option<f64>,
    pub shutter_speed: Option<f64>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub iso_speed_rating: Option<i64>,
    pub date_taken_unix: Option<i64>,
    pub overview: Option<String>,
}

impl PhotoMetadata {
    pub fn from_storage(value: Option<&str>) -> Self {
        value
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default()
    }

    pub fn to_storage(&self) -> Option<String> {
        (self != &Self::default())
            .then(|| serde_json::to_string(self).ok())
            .flatten()
    }
}

pub fn read_photo_metadata(path: &Path) -> PhotoMetadata {
    let Ok(file) = File::open(path) else {
        return PhotoMetadata::default();
    };
    let Ok(exif) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) else {
        return PhotoMetadata::default();
    };

    PhotoMetadata {
        camera_make: ascii_field(&exif, Tag::Make),
        camera_model: ascii_field(&exif, Tag::Model),
        software: ascii_field(&exif, Tag::Software),
        exposure_time: numeric_field(&exif, Tag::ExposureTime),
        focal_length: numeric_field(&exif, Tag::FocalLength),
        image_orientation: uint_field(&exif, Tag::Orientation).and_then(orientation_name),
        aperture: numeric_field(&exif, Tag::ApertureValue),
        shutter_speed: numeric_field(&exif, Tag::ShutterSpeedValue),
        latitude: gps_coordinate(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, 'S'),
        longitude: gps_coordinate(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, 'W'),
        altitude: gps_altitude(&exif),
        iso_speed_rating: uint_field(&exif, Tag::PhotographicSensitivity).map(i64::from),
        date_taken_unix: date_taken_unix(&exif),
        overview: ascii_field(&exif, Tag::ImageDescription),
    }
}

fn field<'a>(exif: &'a Exif, tag: Tag) -> Option<&'a exif::Field> {
    exif.get_field(tag, In::PRIMARY)
}

fn ascii_field(exif: &Exif, tag: Tag) -> Option<String> {
    let Value::Ascii(values) = &field(exif, tag)?.value else {
        return None;
    };
    let value = values.first()?;
    let value = String::from_utf8_lossy(value)
        .trim_matches(['\0', ' '])
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn uint_field(exif: &Exif, tag: Tag) -> Option<u32> {
    field(exif, tag)?.value.get_uint(0)
}

fn numeric_field(exif: &Exif, tag: Tag) -> Option<f64> {
    match &field(exif, tag)?.value {
        Value::Rational(values) => values.first().map(exif::Rational::to_f64),
        Value::SRational(values) => values.first().map(exif::SRational::to_f64),
        Value::Float(values) => values.first().map(|value| f64::from(*value)),
        Value::Double(values) => values.first().copied(),
        value => value.get_uint(0).map(f64::from),
    }
    .filter(|value| value.is_finite())
}

fn orientation_name(value: u32) -> Option<String> {
    Some(
        match value {
            1 => "TopLeft",
            2 => "TopRight",
            3 => "BottomRight",
            4 => "BottomLeft",
            5 => "LeftTop",
            6 => "RightTop",
            7 => "RightBottom",
            8 => "LeftBottom",
            _ => return None,
        }
        .to_string(),
    )
}

fn gps_coordinate(exif: &Exif, tag: Tag, reference_tag: Tag, negative: char) -> Option<f64> {
    let Value::Rational(values) = &field(exif, tag)?.value else {
        return None;
    };
    if values.len() < 3 {
        return None;
    }
    let mut coordinate =
        values[0].to_f64() + values[1].to_f64() / 60.0 + values[2].to_f64() / 3600.0;
    if ascii_field(exif, reference_tag)
        .and_then(|value| value.chars().next())
        .is_some_and(|reference| reference.eq_ignore_ascii_case(&negative))
    {
        coordinate = -coordinate;
    }
    coordinate.is_finite().then_some(coordinate)
}

fn gps_altitude(exif: &Exif) -> Option<f64> {
    let mut altitude = numeric_field(exif, Tag::GPSAltitude)?;
    if uint_field(exif, Tag::GPSAltitudeRef) == Some(1) {
        altitude = -altitude;
    }
    Some(altitude)
}

fn date_taken_unix(exif: &Exif) -> Option<i64> {
    let value =
        ascii_field(exif, Tag::DateTimeOriginal).or_else(|| ascii_field(exif, Tag::DateTime))?;
    let value = value.get(..19)?;
    let date = NaiveDateTime::parse_from_str(value, "%Y:%m:%d %H:%M:%S").ok()?;
    Local
        .from_local_datetime(&date)
        .single()
        .map(|date| date.timestamp())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{orientation_name, read_photo_metadata};

    #[test]
    fn reads_jellyfin_photo_fields_from_exif() {
        let path = std::env::temp_dir().join(format!(
            "jellyfin-rs-photo-exif-{}.jpg",
            uuid::Uuid::new_v4().simple()
        ));
        write_test_photo(&path);

        let metadata = read_photo_metadata(&path);
        assert_eq!(metadata.camera_make.as_deref(), Some("OpenAI Camera"));
        assert_eq!(metadata.camera_model.as_deref(), Some("Codex 1"));
        assert_eq!(metadata.software.as_deref(), Some("jellyfin-rs"));
        assert_eq!(metadata.image_orientation.as_deref(), Some("RightTop"));
        assert_eq!(metadata.exposure_time, Some(1.0 / 125.0));
        assert_eq!(metadata.focal_length, Some(50.0));
        assert_eq!(metadata.aperture, Some(2.8));
        assert_eq!(metadata.shutter_speed, Some(7.0));
        assert_eq!(metadata.iso_speed_rating, Some(200));
        assert_eq!(metadata.latitude, Some(31.25));
        assert!((metadata.longitude.unwrap() - 121.466_666_666_666_67).abs() < 1e-9);
        assert_eq!(metadata.altitude, Some(10.0));
        assert_eq!(metadata.overview.as_deref(), Some("EXIF description"));
        assert!(metadata.date_taken_unix.is_some());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn exif_orientation_values_use_jellyfin_names() {
        assert_eq!(orientation_name(1).as_deref(), Some("TopLeft"));
        assert_eq!(orientation_name(6).as_deref(), Some("RightTop"));
        assert_eq!(orientation_name(8).as_deref(), Some("LeftBottom"));
        assert_eq!(orientation_name(9), None);
    }

    pub(crate) fn write_test_photo(path: &std::path::Path) {
        use exif::{Field, In, Rational, SRational, Tag, Value, experimental::Writer};
        use std::io::Cursor;

        let fields = vec![
            Field {
                tag: Tag::Make,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"OpenAI Camera".to_vec()]),
            },
            Field {
                tag: Tag::Model,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"Codex 1".to_vec()]),
            },
            Field {
                tag: Tag::Software,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"jellyfin-rs".to_vec()]),
            },
            Field {
                tag: Tag::ImageDescription,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"EXIF description".to_vec()]),
            },
            Field {
                tag: Tag::Orientation,
                ifd_num: In::PRIMARY,
                value: Value::Short(vec![6]),
            },
            Field {
                tag: Tag::DateTimeOriginal,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"2024:07:21 10:20:30".to_vec()]),
            },
            Field {
                tag: Tag::ExposureTime,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![Rational { num: 1, denom: 125 }]),
            },
            Field {
                tag: Tag::FocalLength,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![Rational { num: 50, denom: 1 }]),
            },
            Field {
                tag: Tag::ApertureValue,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![Rational { num: 28, denom: 10 }]),
            },
            Field {
                tag: Tag::ShutterSpeedValue,
                ifd_num: In::PRIMARY,
                value: Value::SRational(vec![SRational { num: 7, denom: 1 }]),
            },
            Field {
                tag: Tag::PhotographicSensitivity,
                ifd_num: In::PRIMARY,
                value: Value::Short(vec![200]),
            },
            Field {
                tag: Tag::GPSLatitudeRef,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"N".to_vec()]),
            },
            Field {
                tag: Tag::GPSLatitude,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![
                    Rational { num: 31, denom: 1 },
                    Rational { num: 15, denom: 1 },
                    Rational { num: 0, denom: 1 },
                ]),
            },
            Field {
                tag: Tag::GPSLongitudeRef,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![b"E".to_vec()]),
            },
            Field {
                tag: Tag::GPSLongitude,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![
                    Rational { num: 121, denom: 1 },
                    Rational { num: 28, denom: 1 },
                    Rational { num: 0, denom: 1 },
                ]),
            },
            Field {
                tag: Tag::GPSAltitude,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![Rational { num: 10, denom: 1 }]),
            },
        ];
        let mut writer = Writer::new();
        for field in &fields {
            writer.push_field(field);
        }
        let mut tiff = Cursor::new(Vec::new());
        writer.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();

        let mut jpeg_cursor = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::new(3, 2))
            .write_to(&mut jpeg_cursor, image::ImageFormat::Jpeg)
            .unwrap();
        let jpeg = jpeg_cursor.into_inner();
        let payload_len = 6 + tiff.len();
        let segment_len = u16::try_from(payload_len + 2).unwrap();
        let mut output = Vec::with_capacity(jpeg.len() + payload_len + 4);
        output.extend_from_slice(&jpeg[..2]);
        output.extend_from_slice(&[0xff, 0xe1]);
        output.extend_from_slice(&segment_len.to_be_bytes());
        output.extend_from_slice(b"Exif\0\0");
        output.extend_from_slice(&tiff);
        output.extend_from_slice(&jpeg[2..]);
        std::fs::write(path, output).unwrap();
    }
}
