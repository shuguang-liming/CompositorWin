//! Photoshop descriptors, text engine dictionaries and vector paths.
use super::{Cursor, PsdLimits, PsdRaster};
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

fn unicode(c: &mut Cursor<'_>) -> Result<String> {
    let count = c.u32()? as usize;
    ensure!(count <= 1_000_000, "PSD 描述符字符串超出限制");
    let bytes = c.take(count.checked_mul(2).context("PSD 字符串长度溢出")?)?;
    Ok(String::from_utf16_lossy(
        &bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|x| u16::from_be_bytes([x[0], x[1]]))
            .collect::<Vec<_>>(),
    ))
}
fn identifier(c: &mut Cursor<'_>) -> Result<String> {
    let n = c.u32()? as usize;
    ensure!(n <= 10_000, "PSD 描述符标识符超出限制");
    c.string(if n == 0 { 4 } else { n })
}
fn number(c: &mut Cursor<'_>) -> Result<f64> {
    let n = f64::from_bits(c.u64()?);
    ensure!(n.is_finite(), "PSD 描述符数值不是有限值");
    Ok(n)
}
struct Descriptor<'a> {
    value: Value,
    engine_data: Option<&'a [u8]>,
}

fn raw_data<'a>(c: &mut Cursor<'a>) -> Result<&'a [u8]> {
    let n = c.u32()? as usize;
    ensure!(n <= 8_000_000, "PSD 文字引擎数据过大");
    c.take(n)
}

fn descriptor<'a>(c: &mut Cursor<'a>, versioned: bool, depth: usize) -> Result<Descriptor<'a>> {
    ensure!(depth <= 32, "PSD 描述符递归深度超出限制");
    if versioned {
        ensure!(c.u32()? == 16, "不支持此 PSD 描述符版本");
    }
    let _name = unicode(c)?;
    let _class = identifier(c)?;
    let count = c.u32()?;
    ensure!(count <= 10_000, "PSD 描述符字段过多");
    let mut object = Map::new();
    let mut engine_data = None;
    for _ in 0..count {
        let key = identifier(c)?;
        let kind = c.string(4)?;
        let value = if key == "EngineData" && kind == "tdta" {
            engine_data = Some(raw_data(c)?);
            Value::Null
        } else {
            if key == "EngineData" {
                engine_data = None;
            }
            descriptor_value(c, &kind, depth + 1)?
        };
        object.insert(key, value);
    }
    Ok(Descriptor {
        value: Value::Object(object),
        engine_data,
    })
}
fn descriptor_value(c: &mut Cursor<'_>, kind: &str, depth: usize) -> Result<Value> {
    ensure!(depth <= 32, "PSD 描述符递归深度超出限制");
    Ok(match kind {
        "doub" => json!(number(c)?),
        "UntF" => {
            c.skip(4)?;
            json!(number(c)?)
        }
        "long" => json!(c.i32()?),
        "comp" => json!(c.u64()? as i64),
        "bool" => json!(c.u8()? != 0),
        "TEXT" => json!(unicode(c)?),
        "enum" => {
            identifier(c)?;
            json!(identifier(c)?)
        }
        "Objc" | "GlbO" => descriptor(c, false, depth + 1)?.value,
        "tdta" => {
            raw_data(c)?;
            Value::Null
        }
        "VlLs" => {
            let n = c.u32()?;
            ensure!(n <= 10_000, "PSD 列表过长");
            let mut list = Vec::new();
            for _ in 0..n {
                let t = c.string(4)?;
                list.push(descriptor_value(c, &t, depth + 1)?);
            }
            Value::Array(list)
        }
        "alis" | "Pth " => {
            let n = c.u32()? as usize;
            ensure!(n <= 8_000_000, "PSD 描述符数据过大");
            c.skip(n)?;
            Value::Null
        }
        "type" | "GlbC" => {
            unicode(c)?;
            identifier(c)?;
            Value::Null
        }
        "obj " => {
            let count = c.u32()?;
            ensure!(count <= 10_000, "PSD 引用过长");
            for _ in 0..count {
                let form = c.string(4)?;
                match form.as_str() {
                    "prop" => {
                        unicode(c)?;
                        identifier(c)?;
                        identifier(c)?;
                    }
                    "Clss" => {
                        unicode(c)?;
                        identifier(c)?;
                    }
                    "Enmr" => {
                        unicode(c)?;
                        identifier(c)?;
                        identifier(c)?;
                        identifier(c)?;
                    }
                    "rele" => {
                        unicode(c)?;
                        identifier(c)?;
                        c.i32()?;
                    }
                    "Idnt" | "indx" => {
                        c.i32()?;
                    }
                    "name" => {
                        unicode(c)?;
                    }
                    _ => anyhow::bail!("不支持 PSD 引用类型 {form}"),
                }
            }
            Value::Null
        }
        _ => anyhow::bail!("不支持 PSD 描述符值类型 {kind}"),
    })
}

struct Engine<'a> {
    data: &'a [u8],
    offset: usize,
    tokens: usize,
}
impl Engine<'_> {
    fn whitespace(&mut self) {
        loop {
            while self.data.get(self.offset).is_some_and(|b| *b <= 32) {
                self.offset += 1;
            }
            if self.data.get(self.offset) == Some(&b'%') {
                while self
                    .data
                    .get(self.offset)
                    .is_some_and(|b| *b != b'\n' && *b != b'\r')
                {
                    self.offset += 1;
                }
            } else {
                break;
            }
        }
    }
    fn token(&mut self) -> String {
        let start = self.offset;
        while self
            .data
            .get(self.offset)
            .is_some_and(|b| !b.is_ascii_whitespace() && !b"/<>[]()".contains(b))
        {
            self.offset += 1;
        }
        String::from_utf8_lossy(&self.data[start..self.offset]).into_owned()
    }
    fn parse(&mut self, depth: usize) -> Option<Value> {
        self.tokens += 1;
        if depth > 64 || self.tokens > 200_000 {
            return None;
        }
        self.whitespace();
        let ch = *self.data.get(self.offset)?;
        match ch {
            b'<' if self.data.get(self.offset + 1) == Some(&b'<') => {
                self.offset += 2;
                let mut o = Map::new();
                loop {
                    self.whitespace();
                    if self.data.get(self.offset..self.offset + 2) == Some(b">>") {
                        self.offset += 2;
                        break;
                    }
                    if self.data.get(self.offset) != Some(&b'/') {
                        return None;
                    }
                    self.offset += 1;
                    let key = self.token();
                    o.insert(key, self.parse(depth + 1)?);
                }
                Some(Value::Object(o))
            }
            b'[' => {
                self.offset += 1;
                let mut a = Vec::new();
                loop {
                    self.whitespace();
                    if self.data.get(self.offset) == Some(&b']') {
                        self.offset += 1;
                        break;
                    }
                    a.push(self.parse(depth + 1)?);
                }
                Some(Value::Array(a))
            }
            b'(' => {
                self.offset += 1;
                let mut b = Vec::new();
                let mut nesting = 1;
                while nesting > 0 {
                    let byte = *self.data.get(self.offset)?;
                    self.offset += 1;
                    match byte {
                        b'\\' => {
                            let next = *self.data.get(self.offset)?;
                            self.offset += 1;
                            match next {
                                b'n' => b.push(b'\n'),
                                b'r' => b.push(b'\r'),
                                b't' => b.push(b'\t'),
                                b'\n' | b'\r' => {}
                                b'0'..=b'7' => {
                                    let mut n = next - b'0';
                                    for _ in 0..2 {
                                        if let Some(digit) = self
                                            .data
                                            .get(self.offset)
                                            .filter(|b| (b'0'..=b'7').contains(b))
                                        {
                                            n = n.wrapping_mul(8).wrapping_add(*digit - b'0');
                                            self.offset += 1;
                                        } else {
                                            break;
                                        }
                                    }
                                    b.push(n);
                                }
                                other => b.push(other),
                            }
                        }
                        b'(' => {
                            nesting += 1;
                            b.push(byte);
                        }
                        b')' => {
                            nesting -= 1;
                            if nesting > 0 {
                                b.push(byte);
                            }
                        }
                        other => b.push(other),
                    }
                }
                Some(Value::String(engine_string(&b)))
            }
            b'<' => {
                self.offset += 1;
                let mut nibbles = Vec::new();
                while *self.data.get(self.offset)? != b'>' {
                    let ch = self.data[self.offset] as char;
                    self.offset += 1;
                    if let Some(n) = ch.to_digit(16) {
                        nibbles.push(n as u8);
                    }
                }
                self.offset += 1;
                if nibbles.len() % 2 == 1 {
                    nibbles.push(0);
                }
                Some(Value::String(engine_string(
                    &nibbles
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| p[0] * 16 + p[1])
                        .collect::<Vec<_>>(),
                )))
            }
            b'/' => {
                self.offset += 1;
                Some(Value::String(self.token()))
            }
            _ => {
                let token = self.token();
                if token.is_empty() {
                    return None;
                }
                match token.as_str() {
                    "true" => Some(Value::Bool(true)),
                    "false" => Some(Value::Bool(false)),
                    "null" => Some(Value::Null),
                    _ => {
                        let n = token.parse::<f64>().ok()?;
                        if n.is_finite() { Some(json!(n)) } else { None }
                    }
                }
            }
        }
    }
}
fn engine_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xfe, 0xff]) {
        String::from_utf16_lossy(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        bytes.iter().map(|b| char::from(*b)).collect()
    }
}
fn walk<'a>(value: &'a Value, keys: &[&str]) -> &'a Value {
    let mut at = value;
    for key in keys {
        at = &at[*key];
    }
    at
}

#[derive(Clone, Debug)]
pub struct TextPlacement {
    pub document_anchor: [f64; 2],
    pub rotation: f64,
    pub flip_y: bool,
}
pub fn text(
    extra: &BTreeMap<String, Vec<u8>>,
    notes: &mut Vec<String>,
) -> Option<(Value, TextPlacement)> {
    let data = extra.get("TySh").or_else(|| extra.get("tySh"))?;
    if data.len() > 8_000_000 {
        return None;
    }
    let mut c = Cursor::new(data);
    if c.u16().ok()? != 1 {
        return None;
    }
    let xx = number(&mut c).ok()?;
    let xy = number(&mut c).ok()?;
    let yx = number(&mut c).ok()?;
    let yy = number(&mut c).ok()?;
    let tx = number(&mut c).ok()?;
    let ty = number(&mut c).ok()?;
    let scale = xx.hypot(yx);
    if scale <= 1e-6 {
        return None;
    }
    let cos = xx / scale;
    let sin = yx / scale;
    let shear = cos * xy + sin * yy;
    let local_y = -sin * xy + cos * yy;
    let scale_y = local_y.abs();
    if scale_y <= 1e-6
        || shear.abs() > 0.02 * scale.max(scale_y)
        || (scale - scale_y).abs() > 0.02 * scale.max(scale_y)
    {
        return None;
    }
    if c.u16().ok()? != 50 {
        return None;
    }
    let parsed = descriptor(&mut c, true, 0).ok()?;
    let desc = parsed.value;
    if desc["Ornt"] == "Vrtc" {
        return None;
    }
    if c.bytes.len() - c.offset >= 2
        && c.u16().ok() == Some(1)
        && let Ok(warp) = descriptor(&mut c, true, 0)
        && warp.value["warpStyle"]
            .as_str()
            .is_some_and(|s| s != "warpNone" && s != "none")
    {
        notes.push("Photoshop 文字变形已忽略".to_owned());
    }
    let engine = parsed
        .engine_data
        .and_then(|b| {
            let start = b.windows(2).position(|p| p == b"<<")?;
            Engine {
                data: b,
                offset: start,
                tokens: 0,
            }
            .parse(0)
        })
        .unwrap_or(Value::Null);
    let content = desc["Txt "]
        .as_str()
        .or_else(|| desc["Txt"].as_str())
        .or_else(|| walk(&engine, &["EngineDict", "Editor", "Text"]).as_str())?
        .trim_end_matches(['\0', '\r'])
        .replace('\r', "\n");
    if content.is_empty() || content.encode_utf16().count() > 100_000 {
        return None;
    }
    let runs = walk(&engine, &["EngineDict", "StyleRun", "RunArray"]);
    let first = runs.as_array().and_then(|a| a.first()).unwrap_or(&engine);
    let sheet = walk(first, &["StyleSheet", "StyleSheetData"]);
    if runs.as_array().is_some_and(|r| {
        r.iter()
            .skip(1)
            .any(|v| walk(v, &["StyleSheet", "StyleSheetData"]) != sheet)
    }) {
        notes.push("仅保留了第一个 Photoshop 文字样式".to_owned());
    }
    let font_size = (sheet["FontSize"].as_f64().unwrap_or(12.0) * scale).clamp(1.0, 2000.0);
    let font_index = sheet["Font"].as_f64().unwrap_or(0.0).max(0.0) as usize;
    let font = walk(&engine, &["ResourceDict", "FontSet"])
        .get(font_index)
        .and_then(|v| v["Name"].as_str())
        .unwrap_or("Segoe UI");
    let color = walk(sheet, &["FillColor", "Values"]).as_array();
    let channels = color
        .map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
        .unwrap_or_default();
    let rgb = match channels.as_slice() {
        [_, r, g, b, ..] => [*r, *g, *b],
        [r, g, b] => [*r, *g, *b],
        [g] => [*g; 3],
        _ => [0.0; 3],
    }
    .map(|n| n.clamp(0.0, 1.0));
    let tracking =
        (sheet["Tracking"].as_f64().unwrap_or(0.0) * font_size / 1000.0).clamp(-100.0, 1000.0);
    let leading = if sheet["AutoLeading"] == false {
        (sheet["Leading"].as_f64().unwrap_or(0.0) * scale).clamp(0.0, 5000.0)
    } else {
        0.0
    };
    let justify = walk(&engine, &["EngineDict", "ParagraphRun", "RunArray"])
        .get(0)
        .map(|v| {
            walk(v, &["ParagraphSheet", "Properties", "Justification"])
                .as_f64()
                .unwrap_or(0.0) as i32
        })
        .unwrap_or(0);
    let alignment = match justify {
        1 => "Right",
        2 => "Center",
        _ => "Left",
    };
    if justify > 2 {
        notes.push("完全两端对齐已按左对齐导入".to_owned());
    }
    if sheet["FauxBold"] == true || sheet["FauxItalic"] == true {
        notes.push("Photoshop 伪粗体和伪斜体已忽略".to_owned());
    }
    let mut style = json!({"content":content,"fontName":font,"fontSize":font_size,"red":rgb[0],"green":rgb[1],"blue":rgb[2],"alignment":alignment,"tracking":tracking,"leading":leading});
    let mut document_anchor = [tx, ty];
    let rect = |key: &str| -> Option<[f64; 4]> {
        let v = &desc[key];
        Some([
            v["Left"].as_f64()?,
            v["Top "].as_f64().or_else(|| v["Top"].as_f64())?,
            v["Rght"].as_f64()?,
            v["Btom"].as_f64()?,
        ])
    };
    if let (Some(bounds), Some(glyphs)) = (rect("bounds"), rect("boundingBox")) {
        let w = (bounds[2] - bounds[0]) * scale + 24.0;
        let h = (bounds[3] - bounds[1]) * scale + 24.0;
        if bounds[2] - bounds[0] > glyphs[2] - glyphs[0] + 4.0
            && bounds[3] - bounds[1] > glyphs[3] - glyphs[1] + 4.0
        {
            if !(16.0..=30000.0).contains(&w)
                || !(16.0..=30000.0).contains(&h)
                || w * h > 200_000_000.0
            {
                return None;
            }
            style["boxSize"] = json!([w, h]);
            document_anchor = [
                xx * bounds[0] + xy * bounds[1] + tx,
                yx * bounds[0] + yy * bounds[1] + ty,
            ];
        }
    }
    Some((
        style,
        TextPlacement {
            document_anchor,
            rotation: sin.atan2(cos).to_degrees(),
            flip_y: local_y < 0.0,
        },
    ))
}

fn field(data: &[u8], key: &str, kind: &str) -> Option<usize> {
    data.windows(key.len() + 4)
        .position(|b| b[..key.len()] == *key.as_bytes() && b[key.len()..] == *kind.as_bytes())
        .map(|i| i + key.len() + 4)
}
fn double_field(data: &[u8], key: &str) -> Option<f64> {
    let offset = field(data, key, "doub")?;
    let n = f64::from_be_bytes(data.get(offset..offset + 8)?.try_into().ok()?);
    n.is_finite().then_some(n)
}
fn unit_field(data: &[u8], key: &str) -> Option<f64> {
    let offset = field(data, key, "UntF")? + 4;
    let n = f64::from_be_bytes(data.get(offset..offset + 8)?.try_into().ok()?);
    n.is_finite().then_some(n)
}
fn bool_field(data: &[u8], key: &str) -> Option<bool> {
    Some(*data.get(field(data, key, "bool")?)? != 0)
}
fn color(data: &[u8]) -> Option<[f32; 3]> {
    Some(
        [
            double_field(data, "Rd  ")?,
            double_field(data, "Grn ")?,
            double_field(data, "Bl  ")?,
        ]
        .map(|v| {
            if v > 1.0 {
                (v / 255.0).clamp(0.0, 1.0) as f32
            } else {
                v.clamp(0.0, 1.0) as f32
            }
        }),
    )
}

pub struct Vector {
    pub raster: PsdRaster,
    pub bounds: [i32; 4],
    pub shape: Option<Value>,
    pub notes: Vec<String>,
}
pub fn vector(
    extra: &BTreeMap<String, Vec<u8>>,
    canvas: [u32; 2],
    limits: PsdLimits,
) -> Result<Option<Vector>> {
    use resvg::tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let fill = extra.get("SoCo").and_then(|d| color(d));
    let stroke = extra.get("vstk");
    let fill_enabled = stroke
        .and_then(|d| bool_field(d, "fillEnabled"))
        .unwrap_or(fill.is_some());
    let stroke_enabled = stroke
        .and_then(|d| bool_field(d, "strokeEnabled"))
        .unwrap_or(false);
    if !fill_enabled && !stroke_enabled {
        return Ok(None);
    }
    let mut shape = None;
    let mut notes = Vec::new();
    let mut explicit_bounds = None;
    let mut pb = PathBuilder::new();
    if fill_enabled
        && fill.is_some()
        && let Some(origin) = extra.get("vogk")
        && let Some(at) = field(origin, "keyOriginType", "long")
    {
        let kind = i32::from_be_bytes(
            origin
                .get(at..at + 4)
                .context("矢量类型数据被截断")?
                .try_into()?,
        );
        if [1, 2, 5].contains(&kind)
            && {
                let radii = ["topLeft", "topRight", "bottomRight", "bottomLeft"]
                    .map(|key| unit_field(origin, key));
                let values = radii.into_iter().flatten().collect::<Vec<_>>();
                values.len() != 4
                    || values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                        - values.iter().copied().fold(f64::INFINITY, f64::min)
                        <= 0.5
            }
            && let (Some(left), Some(top), Some(right), Some(bottom)) = (
                unit_field(origin, "Left"),
                unit_field(origin, "Top "),
                unit_field(origin, "Rght"),
                unit_field(origin, "Btom"),
            )
        {
            ensure!(
                (right - left).is_finite()
                    && (bottom - top).is_finite()
                    && right - left <= limits.max_side as f64
                    && bottom - top <= limits.max_side as f64,
                "PSD 矢量形状超出边长限制"
            );
            if right > left && bottom > top {
                let radius = unit_field(origin, "topLeft")
                    .unwrap_or(0.0)
                    .clamp(0.0, ((right - left).min(bottom - top)) / 2.0);
                let rgb = fill.unwrap();
                shape = Some(
                    json!({"kind":if kind==5{"Ellipse"}else{"Rectangle"},"red":rgb[0],"green":rgb[1],"blue":rgb[2],"cornerRadius":radius}),
                );
                explicit_bounds = Some([
                    top.floor() as i32,
                    left.floor() as i32,
                    bottom.ceil() as i32,
                    right.ceil() as i32,
                ]);
                if kind == 5 {
                    if let Some(rect) = resvg::tiny_skia::Rect::from_ltrb(
                        left as f32,
                        top as f32,
                        right as f32,
                        bottom as f32,
                    ) {
                        pb.push_oval(rect);
                    }
                } else {
                    let [l, t, r, b, q] = [left, top, right, bottom, radius].map(|v| v as f32);
                    let arc = q * 0.5522848;
                    pb.move_to(l + q, t);
                    pb.line_to(r - q, t);
                    pb.cubic_to(r - q + arc, t, r, t + q - arc, r, t + q);
                    pb.line_to(r, b - q);
                    pb.cubic_to(r, b - q + arc, r - q + arc, b, r - q, b);
                    pb.line_to(l + q, b);
                    pb.cubic_to(l + q - arc, b, l, b - q + arc, l, b - q);
                    pb.line_to(l, t + q);
                    pb.cubic_to(l, t + q - arc, l + q - arc, t, l + q, t);
                    pb.close();
                }
                if stroke_enabled {
                    notes.push("可编辑形状的 Photoshop 描边已忽略".to_owned());
                }
            }
        }
    }
    if shape.is_none() {
        let Some(data) = extra.get("vmsk").or_else(|| extra.get("vsms")) else {
            return Ok(None);
        };
        if data.len() < 8 {
            return Ok(None);
        }
        let mut remaining = 0;
        let mut closed = true;
        let mut knots = Vec::<[[f32; 2]; 3]>::new();
        let mut sharp = true;
        let flush = |pb: &mut PathBuilder, knots: &mut Vec<[[f32; 2]; 3]>, closed: bool| {
            if let Some(first) = knots.first() {
                pb.move_to(first[1][0], first[1][1]);
                for pair in knots.windows(2) {
                    pb.cubic_to(
                        pair[0][2][0],
                        pair[0][2][1],
                        pair[1][0][0],
                        pair[1][0][1],
                        pair[1][1][0],
                        pair[1][1][1],
                    );
                }
                if closed {
                    let last = knots.last().unwrap();
                    pb.cubic_to(
                        last[2][0],
                        last[2][1],
                        first[0][0],
                        first[0][1],
                        first[1][0],
                        first[1][1],
                    );
                    pb.close();
                }
            }
            knots.clear();
        };
        let mut all_anchors = Vec::new();
        for record in data[8..].as_chunks::<26>().0 {
            let kind = u16::from_be_bytes(record[..2].try_into()?);
            let body = &record[2..];
            match kind {
                0 | 3 => {
                    flush(&mut pb, &mut knots, closed);
                    remaining = u16::from_be_bytes(body[..2].try_into()?);
                    closed = kind == 0;
                }
                1 | 2 | 4 | 5 if remaining > 0 => {
                    remaining -= 1;
                    let mut points = [[0.0; 2]; 3];
                    for (i, p) in points.iter_mut().enumerate() {
                        let y = i32::from_be_bytes(body[i * 8..i * 8 + 4].try_into()?) as f64
                            / 16777216.0;
                        let x = i32::from_be_bytes(body[i * 8 + 4..i * 8 + 8].try_into()?) as f64
                            / 16777216.0;
                        *p = [(x * canvas[0] as f64) as f32, (y * canvas[1] as f64) as f32];
                    }
                    sharp &= points[0] == points[1] && points[1] == points[2];
                    all_anchors.push(points[1]);
                    knots.push(points);
                }
                _ => {}
            }
        }
        flush(&mut pb, &mut knots, closed);
        if sharp
            && all_anchors.len() == 4
            && fill_enabled
            && let Some(rgb) = fill
        {
            let minx = all_anchors
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min);
            let maxx = all_anchors
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max);
            let miny = all_anchors
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min);
            let maxy = all_anchors
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max);
            let corners = all_anchors
                .iter()
                .all(|p| (p[0] == minx || p[0] == maxx) && (p[1] == miny || p[1] == maxy));
            if corners {
                shape = Some(
                    json!({"kind":"Rectangle","red":rgb[0],"green":rgb[1],"blue":rgb[2],"cornerRadius":0}),
                );
            }
        }
    }
    let Some(path) = pb.finish() else {
        return Ok(None);
    };
    let bounds = path.bounds();
    let stroke_width = stroke
        .and_then(|d| unit_field(d, "strokeStyleLineWidth"))
        .unwrap_or(1.0);
    ensure!(
        !stroke_enabled || (0.0..=limits.max_side as f64).contains(&stroke_width),
        "PSD 矢量描边超出限制"
    );
    let inset = if stroke_enabled && shape.is_none() {
        (stroke_width / 2.0 + 1.0).ceil()
    } else {
        0.0
    };
    let box_bounds = explicit_bounds.unwrap_or([
        (bounds.top() as f64 - inset).floor() as i32,
        (bounds.left() as f64 - inset).floor() as i32,
        (bounds.bottom() as f64 + inset).ceil() as i32,
        (bounds.right() as f64 + inset).ceil() as i32,
    ]);
    let (w, h) = super::extent(box_bounds);
    ensure!(
        w > 0
            && h > 0
            && w <= limits.max_side
            && h <= limits.max_side
            && u64::from(w) * u64::from(h) <= limits.document_pixels.min(limits.max_surface_pixels),
        "PSD 矢量内容超出像素预算"
    );
    let mut pixmap = Pixmap::new(w, h).context("PSD 矢量栅格分配失败")?;
    let transform = Transform::from_translate(-box_bounds[1] as f32, -box_bounds[0] as f32);
    if fill_enabled && let Some(rgb) = fill {
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            (rgb[0] * 255.0).round() as u8,
            (rgb[1] * 255.0).round() as u8,
            (rgb[2] * 255.0).round() as u8,
            255,
        );
        pixmap.fill_path(&path, &paint, FillRule::Winding, transform, None);
    }
    if stroke_enabled
        && shape.is_none()
        && let Some(rgb) = stroke.and_then(|d| color(d))
    {
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            (rgb[0] * 255.0).round() as u8,
            (rgb[1] * 255.0).round() as u8,
            (rgb[2] * 255.0).round() as u8,
            255,
        );
        pixmap.stroke_path(
            &path,
            &paint,
            &Stroke {
                width: stroke_width as f32,
                ..Stroke::default()
            },
            transform,
            None,
        );
    }
    let mut pixels = pixmap.take();
    for p in pixels.as_chunks_mut::<4>().0 {
        if p[3] > 0 {
            for i in 0..3 {
                p[i] = ((u32::from(p[i]) * 255 + u32::from(p[3]) / 2) / u32::from(p[3])).min(255)
                    as u8;
            }
        }
    }
    if shape.is_none() {
        notes.push("Photoshop 矢量路径已栅格化".to_owned());
    }
    Ok(Some(Vector {
        raster: PsdRaster {
            width: w,
            height: h,
            data: pixels,
        },
        bounds: box_bounds,
        shape,
        notes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn engine_utf16_and_nested_strings() {
        let bytes=br"<< /EngineDict << /Editor << /Text (\376\377\000A\000B) >> >> /Values [ 1 .5 true ] >>";
        let parsed = Engine {
            data: bytes,
            offset: 0,
            tokens: 0,
        }
        .parse(0)
        .unwrap();
        assert_eq!(parsed["EngineDict"]["Editor"]["Text"], "AB");
        assert_eq!(parsed["Values"][2], true);
    }
    #[test]
    fn descriptor_rejects_depth_before_recursion() {
        assert!(descriptor(&mut Cursor::new(&[]), false, 33).is_err());
    }
}
