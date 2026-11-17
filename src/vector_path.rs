use svgtypes::{PathParser, PathSegment};

use crate::varint;

const PATH_MAGIC: &[u8; 4] = b"HPTH";
const PATH_VERSION: u8 = 1;
const COORDINATE_SCALE: f64 = 64.0;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EncodedPath {
    pub bytes: Vec<u8>,
    pub origin_x: i32,
    pub origin_y: i32,
    pub min_x: i32,
    pub min_y: i32,
    /// Input SVG units represented by one normalized path unit.
    pub scale: f64,
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Bounds {
    fn new() -> Self {
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }

    fn include(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }
}

/// Converts SVG path data to normalized 1/64-unit ZigZag/LEB128 bytecode.
#[cfg(test)]
pub(crate) fn encode_path_data(data: &str) -> Option<EncodedPath> {
    encode_path_data_with_scale_normalization(data, true)
}

pub(crate) fn encode_path_data_unscaled(data: &str) -> Option<EncodedPath> {
    encode_path_data_with_scale_normalization(data, false)
}

fn encode_path_data_with_scale_normalization(
    data: &str,
    normalize_scale: bool,
) -> Option<EncodedPath> {
    let segments = PathParser::from(data).collect::<Result<Vec<_>, _>>().ok()?;
    if segments.is_empty() || segments.len() > u32::MAX as usize {
        return None;
    }

    let (origin_x, origin_y) = match segments.first()? {
        PathSegment::MoveTo { abs: true, x, y } => (quantize(*x)?, quantize(*y)?),
        PathSegment::MoveTo { abs: false, .. } => (0, 0),
        _ => return None,
    };
    let origin_x_units = f64::from(origin_x) / COORDINATE_SCALE;
    let origin_y_units = f64::from(origin_y) / COORDINATE_SCALE;
    let (min_x, min_y, scale, normalization) = if normalize_scale {
        let bounds = local_bounds(&segments, origin_x_units, origin_y_units)?;
        let min_x = quantize(bounds.min_x)?;
        let min_y = quantize(bounds.min_y)?;
        let max_x = quantize(bounds.max_x)?;
        let max_y = quantize(bounds.max_y)?;
        let extent_fixed = (i64::from(max_x) - i64::from(min_x))
            .max(i64::from(max_y) - i64::from(min_y))
            .max(1);
        let extent = extent_fixed as f64 / COORDINATE_SCALE;
        let scale = extent / 1000.0;
        (min_x, min_y, scale, 1.0 / scale)
    } else {
        (0, 0, 1.0, 1.0)
    };

    let mut bytes = Vec::with_capacity(data.len() / 2);
    bytes.extend_from_slice(PATH_MAGIC);
    bytes.push(PATH_VERSION);
    varint::write_u32(segments.len() as u32, &mut bytes);

    for segment in &segments {
        bytes.push(segment.command());
        match *segment {
            PathSegment::MoveTo { abs, x, y } | PathSegment::LineTo { abs, x, y } => {
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                write_coordinate(x1, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y1, abs, origin_y_units, min_y, normalization, &mut bytes)?;
                write_coordinate(x2, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y2, abs, origin_y_units, min_y, normalization, &mut bytes)?;
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                write_coordinate(x2, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y2, abs, origin_y_units, min_y, normalization, &mut bytes)?;
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                write_coordinate(x1, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y1, abs, origin_y_units, min_y, normalization, &mut bytes)?;
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => {
                write_coordinate(rx, false, 0.0, 0, normalization, &mut bytes)?;
                write_coordinate(ry, false, 0.0, 0, normalization, &mut bytes)?;
                varint::write_i32(quantize(x_axis_rotation)?, &mut bytes);
                bytes.push(u8::from(large_arc) | (u8::from(sweep) << 1));
                write_coordinate(x, abs, origin_x_units, min_x, normalization, &mut bytes)?;
                write_coordinate(y, abs, origin_y_units, min_y, normalization, &mut bytes)?;
            }
            PathSegment::ClosePath { .. } => {}
        }
    }

    Some(EncodedPath {
        bytes,
        origin_x,
        origin_y,
        min_x,
        min_y,
        scale,
    })
}

fn write_coordinate(
    value: f64,
    abs: bool,
    origin: f64,
    minimum: i32,
    normalization: f64,
    out: &mut Vec<u8>,
) -> Option<()> {
    let value = if abs {
        (value - origin - f64::from(minimum) / COORDINATE_SCALE) * normalization
    } else {
        value * normalization
    };
    let value = quantize(value)?;
    varint::write_i32(value, out);
    Some(())
}

fn local_bounds(segments: &[PathSegment], origin_x: f64, origin_y: f64) -> Option<Bounds> {
    let mut bounds = Bounds::new();
    let mut current = (0.0, 0.0);
    let mut subpath_start = current;
    for segment in segments {
        match *segment {
            PathSegment::MoveTo { abs, x, y } | PathSegment::LineTo { abs, x, y } => {
                current = point_from_command(current, x, y, abs, origin_x, origin_y);
                bounds.include(current.0, current.1);
                if matches!(*segment, PathSegment::MoveTo { .. }) {
                    subpath_start = current;
                }
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                current.0 = if abs { x - origin_x } else { current.0 + x };
                bounds.include(current.0, current.1);
            }
            PathSegment::VerticalLineTo { abs, y } => {
                current.1 = if abs { y - origin_y } else { current.1 + y };
                bounds.include(current.0, current.1);
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let first = point_from_command(current, x1, y1, abs, origin_x, origin_y);
                let second = point_from_command(current, x2, y2, abs, origin_x, origin_y);
                current = point_from_command(current, x, y, abs, origin_x, origin_y);
                bounds.include(first.0, first.1);
                bounds.include(second.0, second.1);
                bounds.include(current.0, current.1);
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let control = point_from_command(current, x2, y2, abs, origin_x, origin_y);
                current = point_from_command(current, x, y, abs, origin_x, origin_y);
                bounds.include(control.0, control.1);
                bounds.include(current.0, current.1);
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let control = point_from_command(current, x1, y1, abs, origin_x, origin_y);
                current = point_from_command(current, x, y, abs, origin_x, origin_y);
                bounds.include(control.0, control.1);
                bounds.include(current.0, current.1);
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                current = point_from_command(current, x, y, abs, origin_x, origin_y);
                bounds.include(current.0, current.1);
            }
            PathSegment::EllipticalArc {
                abs, rx, ry, x, y, ..
            } => {
                let end = point_from_command(current, x, y, abs, origin_x, origin_y);
                let rx = rx.abs();
                let ry = ry.abs();
                bounds.include(current.0 - rx, current.1 - ry);
                bounds.include(current.0 + rx, current.1 + ry);
                bounds.include(end.0 - rx, end.1 - ry);
                bounds.include(end.0 + rx, end.1 + ry);
                current = end;
            }
            PathSegment::ClosePath { .. } => {
                current = subpath_start;
                bounds.include(current.0, current.1);
            }
        }
    }
    [bounds.min_x, bounds.min_y, bounds.max_x, bounds.max_y]
        .into_iter()
        .all(f64::is_finite)
        .then_some(bounds)
}

fn point_from_command(
    current: (f64, f64),
    x: f64,
    y: f64,
    absolute: bool,
    origin_x: f64,
    origin_y: f64,
) -> (f64, f64) {
    if absolute {
        (x - origin_x, y - origin_y)
    } else {
        (current.0 + x, current.1 + y)
    }
}

fn quantize(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let scaled = (value * COORDINATE_SCALE).round();
    if scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
        return None;
    }
    Some(scaled as i32)
}

#[cfg(test)]
mod tests {
    use super::encode_path_data;

    #[test]
    fn translated_paths_share_the_same_normalized_asset() {
        let first = encode_path_data("M 10 20 C 12 20 14 22 16 24 Z").unwrap();
        let second = encode_path_data("M 30 40 C 32 40 34 42 36 44 Z").unwrap();
        assert_eq!(first.bytes, second.bytes);
        assert_eq!((first.origin_x, first.origin_y), (640, 1280));
        assert_eq!((second.origin_x, second.origin_y), (1920, 2560));
        assert_eq!((first.min_x, first.min_y), (0, 0));
        assert_eq!(first.scale, second.scale);
    }

    #[test]
    fn proportionally_scaled_paths_share_normalized_bytecode() {
        let small = encode_path_data("M 10 20 C 12 20 14 22 16 24 Z").unwrap();
        let large = encode_path_data("M 30 40 C 34 40 38 44 42 48 Z").unwrap();

        assert_eq!(small.bytes, large.bytes);
        assert!((large.scale / small.scale - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_or_invalid_paths_are_left_for_the_svg_fallback() {
        assert!(encode_path_data("").is_none());
        assert!(encode_path_data("M nope").is_none());
    }
}
