//! Parameter projections for Levels and Hue/Saturation canvas interactions.
//!
//! Algorithms follow the original Swift document implementation, licensed under MIT
//! by Wonder Assembly LLC (2026). Raster inputs use straight-alpha sRGB.
use crate::{
    filters,
    model::Raster,
    raw_interaction::{ColorSample, sample_pixel},
};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelsAuto {
    Contrast,
    Color,
    Neutral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelsSample {
    Black,
    Gray,
    White,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HueSampleMode {
    Replace,
    Add,
    Remove,
}

fn section<'a>(settings: &'a Value, key: &str) -> &'a Value {
    settings.get(key).unwrap_or(settings)
}

fn section_mut<'a>(settings: &'a mut Value, key: &str) -> &'a mut Value {
    if settings.get(key).is_some() {
        &mut settings[key]
    } else {
        settings
    }
}

fn object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().unwrap()
}

#[derive(Clone, Copy, Debug)]
struct LevelRange {
    black: f64,
    gamma: f64,
    white: f64,
    output_black: f64,
    output_white: f64,
}

impl Default for LevelRange {
    fn default() -> Self {
        Self {
            black: 0.,
            gamma: 1.,
            white: 255.,
            output_black: 0.,
            output_white: 255.,
        }
    }
}

impl LevelRange {
    fn read(value: &Value) -> Self {
        Self {
            black: filters::number(value, "black", 0.),
            gamma: filters::number(value, "gamma", 1.),
            white: filters::number(value, "white", 255.),
            output_black: filters::number(value, "outputBlack", 0.),
            output_white: filters::number(value, "outputWhite", 255.),
        }
    }

    fn normalized(mut self) -> Self {
        self.black = self.black.clamp(0., 254.);
        self.white = self.white.clamp(self.black + 1., 255.);
        self.gamma = self.gamma.clamp(0.1, 9.99);
        self.output_black = self.output_black.clamp(0., 255.);
        self.output_white = self.output_white.clamp(0., 255.);
        self
    }

    fn json(self) -> Value {
        let normalized = self.normalized();
        json!({
            "black": json_number(normalized.black),
            "gamma": json_number(normalized.gamma),
            "white": json_number(normalized.white),
            "outputBlack": json_number(normalized.output_black),
            "outputWhite": json_number(normalized.output_white),
        })
    }
}

fn json_number(value: f64) -> Value {
    if value.fract() == 0. {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn endpoints(bins: &[u64; 256]) -> Option<(f64, f64)> {
    let total: f64 = bins.iter().map(|&count| count as f64).sum();
    if total == 0. {
        return None;
    }
    let threshold = total * 0.001;
    let mut accumulated = 0.;
    let low = bins.iter().position(|&count| {
        accumulated += count as f64;
        accumulated > threshold
    })?;
    accumulated = 0.;
    let high = bins.iter().rposition(|&count| {
        accumulated += count as f64;
        accumulated > threshold
    })?;
    (low < high).then_some((low as f64, high as f64))
}

/// Bins follow filter_panel's existing order: Red, Green, Blue, composite RGB.
/// The three modes replace all four ranges and select RGB, as the Swift Auto buttons do.
/// Pass cached bins here when available, including selection weights if supplied by the caller.
pub fn apply_levels_auto_bins(
    settings: &mut Value,
    bins: &[[u64; 256]; 4],
    mode: LevelsAuto,
) -> bool {
    let mut ranges = [LevelRange::default(); 4];
    if mode == LevelsAuto::Contrast {
        let limits: Vec<_> = bins[..3].iter().filter_map(endpoints).collect();
        if !limits.is_empty() {
            ranges[0].black = limits.iter().map(|limit| limit.0).fold(255., f64::min);
            ranges[0].white = limits.iter().map(|limit| limit.1).fold(0., f64::max);
        }
    } else {
        for channel in 0..3 {
            let Some((black, white)) = endpoints(&bins[channel]) else {
                continue;
            };
            let mut range = LevelRange {
                black,
                white,
                ..LevelRange::default()
            };
            if mode == LevelsAuto::Neutral {
                let total: f64 = bins[channel].iter().map(|&count| count as f64).sum();
                let mean = bins[channel]
                    .iter()
                    .enumerate()
                    .map(|(index, &count)| {
                        ((index as f64 - black) / (white - black)).clamp(0., 1.) * count as f64
                    })
                    .sum::<f64>()
                    / total;
                if mean > 0. && mean < 1. {
                    range.gamma = (mean.ln() / 0.5_f64.ln()).clamp(0.1, 9.99);
                }
            }
            ranges[channel + 1] = range;
        }
    }
    let original = section(settings, "levels").clone();
    let levels = object(section_mut(settings, "levels"));
    levels.insert("channel".into(), json!("RGB"));
    levels.insert("ranges".into(), json!(ranges.map(LevelRange::json)));
    section(settings, "levels") != &original
}

/// Scans the supplied original sample only on an Auto action. Fully transparent pixels
/// contribute nothing; other pixels contribute their alpha, matching histogram weighting.
pub fn apply_levels_auto(settings: &mut Value, sample: &Raster, mode: LevelsAuto) -> bool {
    let mut bins = [[0; 256]; 4];
    for y in 0..sample.height {
        for x in 0..sample.width {
            let pixel = sample.get(x, y);
            let weight = pixel[3] as u64;
            for channel in 0..3 {
                bins[channel][pixel[channel] as usize] += weight * 3;
                bins[3][pixel[channel] as usize] += weight;
            }
        }
    }
    apply_levels_auto_bins(settings, &bins, mode)
}

/// The point is in original-layer raster pixels, after the caller applies the inverse
/// layer or mask placement transform. Sampling calibrates all three channels together.
pub fn apply_levels_sample(
    settings: &mut Value,
    original: &Raster,
    point: [f64; 2],
    mode: LevelsSample,
) -> bool {
    let Some(sample) = sample_pixel(original, point) else {
        return false;
    };
    let original_settings = section(settings, "levels").clone();
    let existing = original_settings.get("ranges").and_then(Value::as_array);
    let mut ranges: [Value; 4] = std::array::from_fn(|index| {
        existing
            .and_then(|ranges| ranges.get(index))
            .filter(|range| range.is_object())
            .cloned()
            .unwrap_or_else(|| LevelRange::default().json())
    });
    ranges[0] = LevelRange::default().json();
    for (channel, settings) in ranges.iter_mut().enumerate().skip(1) {
        let mut range = LevelRange::read(settings);
        let value = sample.rgb[channel - 1] as f64;
        match mode {
            LevelsSample::Black => range.black = value.max(0.).min(range.white - 1.),
            LevelsSample::White => range.white = value.min(255.).max(range.black + 1.),
            LevelsSample::Gray => {
                let fraction = (value - range.black) / (range.white - range.black);
                if !(fraction > 0. && fraction < 1.) {
                    continue;
                }
                range.gamma = fraction.ln() / 0.5_f64.ln();
            }
        }
        range.output_black = 0.;
        range.output_white = 255.;
        *settings = range.json();
    }
    let levels = object(section_mut(settings, "levels"));
    levels.insert("ranges".into(), json!(ranges));
    section(settings, "levels") != &original_settings
}

const HUE_RANGES: [&str; 7] = [
    "Master", "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas",
];

fn ensure_hue_adjustments(settings: &mut Map<String, Value>) {
    if settings.get("adjustments").is_some_and(Value::is_object) {
        return;
    }
    // The older flat schema applies its values globally. Retain that adjustment on
    // Master when the first range-specific interaction upgrades it to dictionaries.
    let legacy = ["hue", "saturation", "lightness"].map(|key| {
        settings
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(0.)
    });
    settings.insert(
        "adjustments".into(),
        json!({"Master":{"hue":legacy[0],"saturation":legacy[1],"lightness":legacy[2]}}),
    );
}

fn selected_range(settings: &Value) -> usize {
    let name = filters::string(settings, "range", "Master");
    HUE_RANGES
        .iter()
        .position(|&range| range == name)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HueBand {
    falloff_start: f64,
    range_start: f64,
    range_end: f64,
    falloff_end: f64,
}

impl HueBand {
    fn default_for(index: usize) -> Self {
        if index == 0 {
            return Self {
                falloff_start: 0.,
                range_start: 0.,
                range_end: 360.,
                falloff_end: 360.,
            };
        }
        let center = (index - 1) as f64 * 60.;
        Self {
            falloff_start: (center - 45.).rem_euclid(360.),
            range_start: (center - 15.).rem_euclid(360.),
            range_end: (center + 15.).rem_euclid(360.),
            falloff_end: (center + 45.).rem_euclid(360.),
        }
    }

    fn read(settings: &Value, index: usize) -> Self {
        let default = Self::default_for(index);
        let band = &settings["bands"][HUE_RANGES[index]];
        Self {
            falloff_start: filters::number(band, "falloffStart", default.falloff_start),
            range_start: filters::number(band, "rangeStart", default.range_start),
            range_end: filters::number(band, "rangeEnd", default.range_end),
            falloff_end: filters::number(band, "falloffEnd", default.falloff_end),
        }
    }

    fn forward(from: f64, to: f64) -> f64 {
        (to - from).rem_euclid(360.)
    }

    fn weight(self, hue: f64) -> f64 {
        let span = Self::forward(self.falloff_start, self.falloff_end);
        if span <= 0. {
            return 1.;
        }
        let position = Self::forward(self.falloff_start, hue);
        if position > span {
            return 0.;
        }
        let ramp_in = Self::forward(self.falloff_start, self.range_start);
        let plateau_end = Self::forward(self.falloff_start, self.range_end);
        if position < ramp_in {
            if ramp_in > 0. { position / ramp_in } else { 1. }
        } else if position <= plateau_end {
            1.
        } else {
            let ramp_out = span - plateau_end;
            if ramp_out > 0. {
                (span - position) / ramp_out
            } else {
                1.
            }
        }
    }

    fn centered(self, hue: f64) -> Self {
        let core = Self::forward(self.range_start, self.range_end);
        let leading = Self::forward(self.falloff_start, self.range_start);
        let trailing = Self::forward(self.range_end, self.falloff_end);
        let start = (hue - core / 2.).rem_euclid(360.);
        Self {
            falloff_start: (start - leading).rem_euclid(360.),
            range_start: start,
            range_end: (start + core).rem_euclid(360.),
            falloff_end: (start + core + trailing).rem_euclid(360.),
        }
    }

    fn include(&mut self, hue: f64) {
        if self.weight(hue) >= 1. {
            return;
        }
        let leading = Self::forward(self.falloff_start, self.range_start);
        let trailing = Self::forward(self.range_end, self.falloff_end);
        if Self::forward(hue, self.range_start) <= Self::forward(self.range_end, hue) {
            self.range_start = hue;
            self.falloff_start = hue - leading;
        } else {
            self.range_end = hue;
            self.falloff_end = hue + trailing;
        }
        self.normalize();
    }

    fn exclude(&mut self, hue: f64) {
        if self.weight(hue) <= 0. {
            return;
        }
        let leading = Self::forward(self.falloff_start, self.range_start);
        let trailing = Self::forward(self.range_end, self.falloff_end);
        if Self::forward(self.falloff_start, hue) <= Self::forward(hue, self.falloff_end) {
            self.falloff_start = hue + 1.;
            self.range_start = hue + 1. + leading;
        } else {
            self.falloff_end = hue - 1.;
            self.range_end = hue - 1. - trailing;
        }
        self.normalize();
    }

    fn normalize(&mut self) {
        self.falloff_start = self.falloff_start.rem_euclid(360.);
        self.range_start = self.range_start.rem_euclid(360.);
        self.range_end = self.range_end.rem_euclid(360.);
        self.falloff_end = self.falloff_end.rem_euclid(360.);
        if Self::forward(self.falloff_start, self.falloff_end) > 350. {
            self.falloff_end = (self.falloff_start + 350.).rem_euclid(360.);
        }
    }

    fn json(self) -> Value {
        json!({
            "falloffStart": self.falloff_start,
            "rangeStart": self.range_start,
            "rangeEnd": self.range_end,
            "falloffEnd": self.falloff_end,
        })
    }
}

/// The source must be the visible composite; its point is in that Raster's pixel space.
/// Master, Colorize, transparent, out-of-bounds, and near-neutral samples are ignored.
pub fn apply_hue_sample(
    settings: &mut Value,
    composite: &Raster,
    point: [f64; 2],
    mode: HueSampleMode,
) -> bool {
    let Some(sample) = sample_pixel(composite, point) else {
        return false;
    };
    apply_hue_color(settings, sample, mode)
}

fn valid_hue_sample(sample: ColorSample) -> bool {
    [sample.hue, sample.saturation, sample.tone, sample.luminance]
        .iter()
        .all(|value| value.is_finite())
        && sample.saturation > 0.02
}

/// Accepts an already sampled visible composite pixel, avoiding a full composite
/// Raster allocation when the caller uses render::CompositeSampler::sample.
pub fn apply_hue_color(settings: &mut Value, sample: ColorSample, mode: HueSampleMode) -> bool {
    let hue_settings = section(settings, "hsvSettings");
    let index = selected_range(hue_settings);
    if index == 0 || filters::boolean(hue_settings, "colorize", false) || !valid_hue_sample(sample)
    {
        return false;
    }
    let before = HueBand::read(hue_settings, index);
    let mut after = before;
    match mode {
        HueSampleMode::Replace => after = after.centered(sample.hue),
        HueSampleMode::Add => after.include(sample.hue),
        HueSampleMode::Remove => after.exclude(sample.hue),
    }
    if after == before {
        return false;
    }
    let hue_settings = object(section_mut(settings, "hsvSettings"));
    ensure_hue_adjustments(hue_settings);
    let bands = object(hue_settings.entry("bands").or_insert_with(|| json!({})));
    bands.insert(HUE_RANGES[index].into(), after.json());
    true
}

/// An absolute horizontal drag. Keeping the initial channel values prevents accumulation.
#[derive(Clone, Debug)]
pub struct HueTargetDrag {
    pub range: &'static str,
    pub hue: f64,
    pub saturation: f64,
    pub start_x: f64,
}

impl HueTargetDrag {
    /// Chooses the strongest color-family weight, including selected-band inversion.
    /// A valid beginning updates the selected range. Equal weights retain the first range.
    pub fn begin(
        settings: &mut Value,
        composite: &Raster,
        point: [f64; 2],
        start_x: f64,
    ) -> Option<Self> {
        let sample = sample_pixel(composite, point)?;
        Self::begin_color(settings, sample, start_x)
    }

    /// Begins from one already sampled composite pixel without allocating an image.
    pub fn begin_color(settings: &mut Value, sample: ColorSample, start_x: f64) -> Option<Self> {
        let hue_settings = section(settings, "hsvSettings");
        if !start_x.is_finite()
            || filters::boolean(hue_settings, "colorize", false)
            || !valid_hue_sample(sample)
        {
            return None;
        }
        let selected = selected_range(hue_settings);
        let inverted = filters::boolean(hue_settings, "invertRange", false);
        let mut best = 1;
        let mut best_weight = -1.;
        for index in 1..HUE_RANGES.len() {
            let mut weight = HueBand::read(hue_settings, index).weight(sample.hue);
            if inverted && index == selected {
                weight = 1. - weight;
            }
            if weight > best_weight {
                best = index;
                best_weight = weight;
            }
        }
        let range = HUE_RANGES[best];
        let adjustment = &hue_settings["adjustments"][range];
        let drag = Self {
            range,
            hue: filters::number(adjustment, "hue", 0.),
            saturation: filters::number(adjustment, "saturation", 0.),
            start_x,
        };
        object(section_mut(settings, "hsvSettings")).insert("range".into(), json!(range));
        Some(drag)
    }

    /// Mirrors Swift's delta/2 behavior. The modifier selects hue versus saturation;
    /// the other component and any settings edited during this drag are retained.
    pub fn apply(&self, settings: &mut Value, current_x: f64, adjusts_hue: bool) -> bool {
        if !current_x.is_finite() || !self.start_x.is_finite() {
            return false;
        }
        let delta = (current_x - self.start_x) / 2.;
        let (key, value) = if adjusts_hue {
            ("hue", (self.hue + delta).clamp(-180., 180.))
        } else {
            ("saturation", (self.saturation + delta).clamp(-100., 100.))
        };
        let before = section(settings, "hsvSettings").clone();
        let hue_settings = object(section_mut(settings, "hsvSettings"));
        ensure_hue_adjustments(hue_settings);
        let adjustments = object(
            hue_settings
                .entry("adjustments")
                .or_insert_with(|| json!({})),
        );
        let adjustment = object(adjustments.entry(self.range).or_insert_with(|| json!({})));
        adjustment.insert(key.into(), json!(value));
        section(settings, "hsvSettings") != &before
    }

    pub fn settings_at(&self, current: &Value, current_x: f64, adjusts_hue: bool) -> Value {
        let mut result = current.clone();
        self.apply(&mut result, current_x, adjusts_hue);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_ranges(settings: &Value) -> &Vec<Value> {
        section(settings, "levels")["ranges"].as_array().unwrap()
    }

    #[test]
    fn auto_contrast_uses_shared_extrema_and_color_uses_each_channel() {
        let source = Raster::from_rgba(2, 1, vec![20, 50, 80, 255, 100, 150, 200, 255]);
        let mut contrast = filters::default_settings("Levels");
        contrast["kind"] = json!("Levels");
        contrast["metadata"] = json!(17);
        assert!(apply_levels_auto(
            &mut contrast,
            &source,
            LevelsAuto::Contrast
        ));
        assert_eq!(level_ranges(&contrast)[0]["black"], 20.);
        assert_eq!(level_ranges(&contrast)[0]["white"], 200.);
        assert_eq!(contrast["metadata"], 17);
        for range in &level_ranges(&contrast)[1..] {
            assert_eq!(*range, LevelRange::default().json());
        }
        let mut color = filters::default_settings("Levels");
        assert!(apply_levels_auto(&mut color, &source, LevelsAuto::Color));
        for (index, (black, white)) in [(20., 100.), (50., 150.), (80., 200.)]
            .into_iter()
            .enumerate()
        {
            assert_eq!(level_ranges(&color)[index + 1]["black"], black);
            assert_eq!(level_ranges(&color)[index + 1]["white"], white);
        }
        assert_eq!(level_ranges(&color)[0], LevelRange::default().json());
        assert!(!apply_levels_auto(&mut color, &source, LevelsAuto::Color));
    }

    #[test]
    fn auto_tail_is_strict_one_per_thousand_and_neutral_gamma_calibrates_means() {
        let mut bins = [[0; 256]; 4];
        for channel in &mut bins[..3] {
            channel[0] = 1;
            channel[20] = 499;
            channel[200] = 499;
            channel[255] = 1;
        }
        let mut settings = filters::default_settings("Levels");
        apply_levels_auto_bins(&mut settings, &bins, LevelsAuto::Color);
        assert_eq!(level_ranges(&settings)[1]["black"], 20.);
        assert_eq!(level_ranges(&settings)[1]["white"], 200.);
        bins = [[0; 256]; 4];
        bins[0][0] = 3;
        bins[0][255] = 1;
        bins[1][0] = 1;
        bins[1][255] = 3;
        bins[2][0] = 1;
        bins[2][255] = 1;
        apply_levels_auto_bins(&mut settings, &bins, LevelsAuto::Neutral);
        assert!((filters::number(&level_ranges(&settings)[1], "gamma", 0.) - 2.).abs() < 1e-12);
        assert!(
            (filters::number(&level_ranges(&settings)[2], "gamma", 0.)
                - 0.75_f64.ln() / 0.5_f64.ln())
            .abs()
                < 1e-12
        );
        assert_eq!(level_ranges(&settings)[3]["gamma"], 1.);
    }

    #[test]
    fn transparent_pixels_and_flat_channels_do_not_supply_auto_endpoints() {
        let source = Raster::from_rgba(3, 1, vec![0, 0, 0, 0, 80, 90, 100, 127, 80, 90, 100, 255]);
        let mut settings = filters::default_settings("Levels");
        settings["levels"]["ranges"][0]["black"] = json!(45);
        assert!(apply_levels_auto(&mut settings, &source, LevelsAuto::Color));
        assert_eq!(settings, filters::default_settings("Levels"));
    }

    #[test]
    fn sample_black_white_resets_composite_and_output_ranges_but_retains_channel() {
        let source = Raster::solid(1, 1, [200, 40, 100, 128]);
        let mut settings = filters::default_settings("Levels");
        settings["levels"]["channel"] = json!("Blue");
        settings["levels"]["ranges"][0]["black"] = json!(30);
        settings["levels"]["ranges"][1]["white"] = json!(100);
        settings["levels"]["ranges"][2]["outputBlack"] = json!(60);
        apply_levels_sample(&mut settings, &source, [0., 0.], LevelsSample::Black);
        assert_eq!(level_ranges(&settings)[0], LevelRange::default().json());
        assert_eq!(level_ranges(&settings)[1]["black"], 99.);
        assert_eq!(level_ranges(&settings)[2]["black"], 40.);
        assert_eq!(level_ranges(&settings)[2]["outputBlack"], 0.);
        assert_eq!(settings["levels"]["channel"], "Blue");
        apply_levels_sample(&mut settings, &source, [0., 0.], LevelsSample::White);
        assert_eq!(level_ranges(&settings)[1]["white"], 200.);
        assert_eq!(level_ranges(&settings)[2]["white"], 41.);
    }

    #[test]
    fn gray_sample_skips_endpoints_and_keeps_their_existing_output_mapping() {
        let source = Raster::solid(1, 1, [128, 64, 0, 255]);
        let mut settings = filters::default_settings("Levels");
        settings["levels"]["ranges"][3]["outputBlack"] = json!(42);
        settings["levels"]["ranges"][3]["outputWhite"] = json!(57);
        apply_levels_sample(&mut settings, &source, [0., 0.], LevelsSample::Gray);
        assert!(
            (filters::number(&level_ranges(&settings)[1], "gamma", 0.)
                - (128_f64 / 255.).ln() / 0.5_f64.ln())
            .abs()
                < 1e-12
        );
        assert_eq!(level_ranges(&settings)[3]["outputBlack"], 42);
        assert_eq!(level_ranges(&settings)[3]["outputWhite"], 57);
    }

    #[test]
    fn missing_and_transparent_samples_leave_settings_untouched() {
        let source = Raster::solid(1, 1, [255, 0, 0, 0]);
        let mut settings = filters::default_settings("Levels");
        let before = settings.clone();
        for point in [[0., 0.], [-0.01, 0.], [1., 0.], [f64::NAN, 0.]] {
            assert!(!apply_levels_sample(
                &mut settings,
                &source,
                point,
                LevelsSample::Black
            ));
            assert_eq!(settings, before);
        }
    }

    #[test]
    fn hue_sampling_preserves_shoulders_across_wrap_and_ignores_no_op_add() {
        let mut settings = filters::default_settings("Hue/Saturation");
        settings["hsvSettings"]["range"] = json!("Reds");
        let green = Raster::solid(1, 1, [0, 255, 0, 255]);
        assert!(apply_hue_sample(
            &mut settings,
            &green,
            [0., 0.],
            HueSampleMode::Replace
        ));
        assert_eq!(
            settings["hsvSettings"]["bands"]["Reds"],
            json!({"falloffStart":75.,"rangeStart":105.,"rangeEnd":135.,"falloffEnd":165.})
        );
        let before = settings.clone();
        assert!(!apply_hue_sample(
            &mut settings,
            &green,
            [0., 0.],
            HueSampleMode::Add
        ));
        assert_eq!(settings, before);
        let mut band = HueBand::default_for(1);
        band.include(60.);
        assert_eq!(
            band.json(),
            json!({"falloffStart":315.,"rangeStart":345.,"rangeEnd":60.,"falloffEnd":90.})
        );
        band = HueBand::default_for(1);
        band.exclude(330.);
        assert_eq!(
            band.json(),
            json!({"falloffStart":331.,"rangeStart":1.,"rangeEnd":15.,"falloffEnd":45.})
        );
        assert_eq!(band.weight(330.), 0.);
    }

    #[test]
    fn hue_sampling_requires_a_color_family_non_neutral_composite_and_no_colorize() {
        let neutral = Raster::solid(1, 1, [100, 101, 100, 255]);
        let green = Raster::solid(1, 1, [0, 255, 0, 255]);
        let mut settings = filters::default_settings("Hue/Saturation");
        assert!(!apply_hue_sample(
            &mut settings,
            &green,
            [0., 0.],
            HueSampleMode::Replace
        ));
        settings["hsvSettings"]["range"] = json!("Reds");
        let before = settings.clone();
        assert!(!apply_hue_sample(
            &mut settings,
            &neutral,
            [0., 0.],
            HueSampleMode::Replace
        ));
        assert_eq!(settings, before);
        settings["hsvSettings"]["colorize"] = json!(true);
        assert!(!apply_hue_sample(
            &mut settings,
            &green,
            [0., 0.],
            HueSampleMode::Replace
        ));
        assert!(HueTargetDrag::begin(&mut settings, &green, [0., 0.], 0.).is_none());
    }

    #[test]
    fn targeted_drag_selects_family_projects_absolute_delta_and_preserves_other_edits() {
        let cyan = Raster::solid(1, 1, [0, 255, 255, 255]);
        let mut settings = filters::default_settings("Hue/Saturation");
        settings["hsvSettings"]["adjustments"]["Cyans"]["hue"] = json!(17);
        settings["hsvSettings"]["adjustments"]["Cyans"]["saturation"] = json!(-12);
        settings["metadata"] = json!(91);
        let drag = HueTargetDrag::begin(&mut settings, &cyan, [0., 0.], 100.).unwrap();
        assert_eq!(drag.range, "Cyans");
        assert_eq!(settings["hsvSettings"]["range"], "Cyans");
        drag.apply(&mut settings, 120., false);
        assert_eq!(
            settings["hsvSettings"]["adjustments"]["Cyans"]["saturation"],
            -2.
        );
        drag.apply(&mut settings, 140., false);
        assert_eq!(
            settings["hsvSettings"]["adjustments"]["Cyans"]["saturation"],
            8.
        );
        settings["hsvSettings"]["adjustments"]["Cyans"]["lightness"] = json!(30);
        drag.apply(&mut settings, 160., true);
        assert_eq!(settings["hsvSettings"]["adjustments"]["Cyans"]["hue"], 47.);
        assert_eq!(
            settings["hsvSettings"]["adjustments"]["Cyans"]["saturation"],
            8.
        );
        assert_eq!(
            settings["hsvSettings"]["adjustments"]["Cyans"]["lightness"],
            30
        );
        assert_eq!(settings["metadata"], 91);
        drag.apply(&mut settings, 10000., true);
        assert_eq!(settings["hsvSettings"]["adjustments"]["Cyans"]["hue"], 180.);
        let before = settings.clone();
        assert!(!drag.apply(&mut settings, f64::NAN, false));
        assert_eq!(settings, before);
    }

    #[test]
    fn targeted_range_choice_honors_selected_band_inversion_and_first_tie() {
        let green = Raster::solid(1, 1, [0, 255, 0, 255]);
        let mut settings = filters::default_settings("Hue/Saturation");
        settings["hsvSettings"]["range"] = json!("Reds");
        settings["hsvSettings"]["invertRange"] = json!(true);
        let drag = HueTargetDrag::begin(&mut settings, &green, [0., 0.], 0.).unwrap();
        assert_eq!(drag.range, "Reds");
        assert_eq!(settings["hsvSettings"]["range"], "Reds");
    }

    #[test]
    fn direct_settings_and_legacy_hue_values_keep_metadata_and_global_grade() {
        let cyan = Raster::solid(1, 1, [0, 255, 255, 255]);
        let mut legacy = json!({"hue":15,"saturation":-20,"lightness":8,"metadata":42});
        let original_render = filters::apply_filter(&cyan, "Hue/Saturation", &legacy).unwrap();
        let drag = HueTargetDrag::begin(&mut legacy, &cyan, [0., 0.], 0.).unwrap();
        drag.apply(&mut legacy, 0., false);
        assert_eq!(legacy["metadata"], 42);
        assert_eq!(legacy["adjustments"]["Master"]["hue"], 15.);
        assert_eq!(legacy["adjustments"]["Master"]["saturation"], -20.);
        assert_eq!(
            filters::apply_filter(&cyan, "Hue/Saturation", &legacy).unwrap(),
            original_render
        );
        let mut levels = filters::default_settings("Levels")["levels"].clone();
        levels["metadata"] = json!(33);
        apply_levels_sample(&mut levels, &cyan, [0., 0.], LevelsSample::White);
        assert_eq!(levels["metadata"], 33);
        assert!(levels.get("levels").is_none());
        assert_eq!(levels["ranges"][1]["white"], 1.);
    }

    #[test]
    fn sampled_color_interfaces_match_raster_projections_and_reject_invalid_components() {
        let source = Raster::solid(1, 1, [0, 255, 0, 255]);
        let sample = ColorSample::from_rgba([0, 255, 0, 255]).unwrap();
        let mut direct = filters::default_settings("Hue/Saturation");
        direct["hsvSettings"]["range"] = json!("Reds");
        let mut from_raster = direct.clone();
        assert_eq!(
            apply_hue_color(&mut direct, sample, HueSampleMode::Replace),
            apply_hue_sample(&mut from_raster, &source, [0., 0.], HueSampleMode::Replace)
        );
        assert_eq!(direct, from_raster);
        let direct_drag = HueTargetDrag::begin_color(&mut direct, sample, 25.).unwrap();
        let raster_drag = HueTargetDrag::begin(&mut from_raster, &source, [0., 0.], 25.).unwrap();
        assert_eq!(direct_drag.range, raster_drag.range);
        assert_eq!(direct, from_raster);
        for invalid in [
            ColorSample {
                hue: f64::NAN,
                ..sample
            },
            ColorSample {
                saturation: f64::INFINITY,
                ..sample
            },
            ColorSample {
                tone: f64::NAN,
                ..sample
            },
            ColorSample {
                luminance: f64::NEG_INFINITY,
                ..sample
            },
            ColorSample {
                saturation: 0.02,
                ..sample
            },
        ] {
            let before = direct.clone();
            assert!(!apply_hue_color(
                &mut direct,
                invalid,
                HueSampleMode::Replace
            ));
            assert!(HueTargetDrag::begin_color(&mut direct, invalid, 0.).is_none());
            assert_eq!(direct, before);
        }
        assert!(HueTargetDrag::begin_color(&mut direct, sample, f64::NAN).is_none());
    }
}
