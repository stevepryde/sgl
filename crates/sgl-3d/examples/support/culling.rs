//! What the views' draw lists cost on an example's route (#24), measured by
//! the `streaming`, `browser_streaming` and `irradiance_volume` examples:
//! the CPU time each GPU-built view's draw list takes to build (preparing
//! its cull and encoding it) and record, and the GPU time of each pass
//! group, the cull stage's among them. `--split` runs the opaque stage's
//! two-pass form instead of the fused pass. The examples include this file
//! by path.
use sgl_3d::Renderer;
use sgl_3d::diagnostics::ViewTimes;
use sgl_3d::settings::Settings;
use sgl_3d::timing::FrameTime;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The measurement's command-line options.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// The opaque stage's two-pass form: a G-buffer pass, then lighting at
    /// its depth (`DisabledLayers::fused_opaque`).
    pub split: bool,
}

impl Options {
    /// Takes `arg` when it is one of the options.
    pub fn take(&mut self, arg: &str) -> bool {
        match arg {
            "--split" => self.split = true,
            _ => return false,
        }
        true
    }

    /// Sets `settings` for the run.
    pub fn apply(self, settings: &mut Settings) {
        settings.diagnostics.disable.fused_opaque = self.split;
    }

    /// What the run renders, for its report.
    pub fn describe(self) -> String {
        format!("opaque {}", if self.split { "two-pass" } else { "fused" })
    }
}

/// The opaque stage's timing groups before ambient occlusion, fused and
/// two-pass.
const OPAQUE: [&str; 4] = [
    "sky",
    "opaque geometry + lighting",
    "geometry",
    "opaque lighting",
];
/// The cull stage's timing group.
const CULL: &str = "cull";

/// One frame's GPU time: its total and each pass group's.
type GpuFrame = (f64, BTreeMap<&'static str, f64>);

/// What the measured frames recorded.
#[derive(Default)]
pub struct Culling {
    options: Options,
    /// Whether each loop index so far is measured.
    measured: Vec<bool>,
    views: Vec<ViewTimes>,
    render_ms: Vec<f64>,
    finish_ms: Vec<f64>,
    gpu: Vec<GpuFrame>,
}

impl Culling {
    pub fn new(options: Options) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// After the frame of loop index `index` was submitted and finished,
    /// `measure` it or not, which took `render_ms` in `Renderer::render`
    /// and `finish_ms` finishing its encoder.
    pub fn frame(
        &mut self,
        renderer: &Renderer,
        index: usize,
        measure: bool,
        (render_ms, finish_ms): (f64, f64),
    ) {
        self.measured.resize(index + 1, false);
        self.measured[index] = measure;
        if !measure {
            return;
        }
        self.views.push(renderer.diagnostic_view_times());
        self.render_ms.push(render_ms);
        self.finish_ms.push(finish_ms);
    }

    /// A completed frame's GPU time; `begin_frame` numbers loop index i's
    /// frame i + 1.
    pub fn gpu(&mut self, frame: &FrameTime) {
        let Some(index) = (frame.frame as usize).checked_sub(1) else {
            return;
        };
        if !self.measured.get(index).copied().unwrap_or(false) {
            return;
        }
        let mut groups = BTreeMap::new();
        for pass in &frame.passes {
            *groups.entry(pass.name).or_insert(0.) += pass.ms;
        }
        self.gpu.push((frame.total_ms, groups));
    }

    /// The report, a line each.
    pub fn report(&self) -> String {
        let out = std::cell::RefCell::new(String::new());
        let print = |text: String| {
            let _ = writeln!(out.borrow_mut(), "{text}");
        };
        print(format!("  {}", self.options.describe()));
        let line = |label: &str, values: &[f64], unit: &str| {
            let (median, p95) = median_p95(values);
            print(format!("  {label:<40} {median:10.3} / {p95:10.3} {unit}"));
        };
        print(format!(
            "  CPU per view over {} frames, median / p95:",
            self.views.len()
        ));
        line(
            "camera build",
            &self
                .views
                .iter()
                .map(|v| v.camera.build_ms)
                .collect::<Vec<_>>(),
            "ms",
        );
        line(
            "camera encode",
            &self
                .views
                .iter()
                .map(|v| v.camera.encode_ms)
                .collect::<Vec<_>>(),
            "ms",
        );
        let cascades = self
            .views
            .iter()
            .map(|v| v.cascades.len())
            .max()
            .unwrap_or(0);
        for cascade in 0..cascades {
            let pick = |time: fn(&sgl_3d::diagnostics::ViewTime) -> f64| -> Vec<f64> {
                self.views
                    .iter()
                    .map(|v| v.cascades.get(cascade).map_or(0., time))
                    .collect()
            };
            line(
                &format!("cascade {cascade} build"),
                &pick(|t| t.build_ms),
                "ms",
            );
            line(
                &format!("cascade {cascade} encode"),
                &pick(|t| t.encode_ms),
                "ms",
            );
        }
        line("Renderer::render", &self.render_ms, "ms");
        line("CommandEncoder::finish", &self.finish_ms, "ms");
        if self.gpu.is_empty() {
            return out.into_inner();
        }
        let mut names: Vec<&str> = Vec::new();
        for (_, groups) in &self.gpu {
            for name in groups.keys() {
                if !names.contains(name) {
                    names.push(name);
                }
            }
        }
        print(format!(
            "  GPU per pass group over {} frames, median / p95 ms:",
            self.gpu.len()
        ));
        let row = |label: &str, pick: &dyn Fn(&GpuFrame) -> f64| {
            let (median, p95) = median_p95(&self.gpu.iter().map(pick).collect::<Vec<_>>());
            print(format!("  {label:<40} {median:8.3} / {p95:8.3}"));
        };
        let sum = |groups: &BTreeMap<&str, f64>, of: &dyn Fn(&str) -> bool| -> f64 {
            groups
                .iter()
                .filter(|(name, _)| of(name))
                .map(|(_, ms)| ms)
                .sum()
        };
        row("frame", &|(total, _)| *total);
        row("opaque stage (sky, geometry, lighting)", &|(_, groups)| {
            sum(groups, &|name| OPAQUE.contains(&name))
        });
        row("directional shadow cascades", &|(_, groups)| {
            sum(groups, &|name| {
                name.starts_with("directional shadow cascade")
            })
        });
        row("cull + opaque stage + cascades", &|(_, groups)| {
            sum(groups, &|name| {
                name == CULL
                    || OPAQUE.contains(&name)
                    || name.starts_with("directional shadow cascade")
            })
        });
        for name in names {
            row(name, &|(_, groups)| groups.get(name).copied().unwrap_or(0.));
        }
        out.into_inner()
    }
}

/// The median and 95th percentile of `values`.
fn median_p95(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0., 0.);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    (at(0.5), at(0.95))
}
