//! What occlusion culling could save on an example's route (#24), measured
//! by the `streaming` and `irradiance_volume` examples: the CPU time each
//! view's draw list takes to build and record, and, with `--visibility`,
//! the share of the camera's opaque and masked instances and triangles a
//! frame drew without a pixel and the GPU time of each pass group in the
//! frames that drew every instance against those that skipped the hidden
//! ones (`InstanceVisibility`). `--split` runs the opaque stage's two-pass
//! form instead of the fused pass. Both examples include this file by path.
use sgl_3d::Renderer;
use sgl_3d::diagnostics::{InstanceVisibilityReport, ViewTimes};
use sgl_3d::settings::{InstanceVisibility, Settings};
use sgl_3d::timing::FrameTime;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

/// The measurement's command-line options.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// The opaque stage's two-pass form: a G-buffer pass, then lighting at
    /// its depth (`DisabledLayers::fused_opaque`).
    pub split: bool,
    /// Frames alternate between observing the camera's instance visibility
    /// and skipping the hidden instances.
    pub visibility: bool,
}

impl Options {
    /// Takes `arg` when it is one of the options.
    pub fn take(&mut self, arg: &str) -> bool {
        match arg {
            "--split" => self.split = true,
            "--visibility" => self.visibility = true,
            _ => return false,
        }
        true
    }

    /// Sets `settings` for the frame of loop index `index`: even frames
    /// draw every instance and observe, odd ones skip the hidden instances.
    pub fn apply(self, settings: &mut Settings, index: usize) {
        settings.diagnostics.disable.fused_opaque = self.split;
        settings.diagnostics.instance_visibility = match (self.visibility, index % 2) {
            (false, _) => InstanceVisibility::Off,
            (true, 0) => InstanceVisibility::Observe,
            (true, _) => InstanceVisibility::SkipHidden,
        };
    }

    /// What the run renders, for its report.
    pub fn describe(self) -> String {
        format!(
            "opaque {}{}",
            if self.split { "two-pass" } else { "fused" },
            if self.visibility {
                ", even frames drawing every instance, odd frames skipping the hidden ones"
            } else {
                ""
            }
        )
    }
}

/// Frames that drew every instance, then those that skipped the hidden.
const KINDS: [&str; 2] = ["all drawn", "hidden skipped"];
/// The opaque stage's timing groups before ambient occlusion, fused and
/// two-pass, and the instance visibility pass's.
const OPAQUE: [&str; 4] = [
    "sky",
    "opaque geometry + lighting",
    "geometry",
    "opaque lighting",
];
const VISIBILITY: &str = "instance visibility";

/// One frame's GPU time: its total and each pass group's.
type GpuFrame = (f64, BTreeMap<&'static str, f64>);

/// What the measured frames recorded, by kind of frame.
#[derive(Default)]
pub struct Culling {
    options: Options,
    /// Whether each loop index so far is measured.
    measured: Vec<bool>,
    /// The observed frames whose visibility has yet to arrive, in order.
    observed: VecDeque<usize>,
    reports: Vec<InstanceVisibilityReport>,
    views: Vec<ViewTimes>,
    render_ms: Vec<f64>,
    finish_ms: Vec<f64>,
    triangles: [Vec<f64>; 2],
    gpu: [Vec<GpuFrame>; 2],
}

impl Culling {
    pub fn new(options: Options) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    fn kind(&self, index: usize) -> usize {
        usize::from(self.options.visibility && index % 2 == 1)
    }

    /// After the frame of loop index `index` was submitted and finished,
    /// `measure` it or not, which took `render_ms` in `Renderer::render`
    /// and `finish_ms` finishing its encoder.
    pub fn frame(
        &mut self,
        (renderer, device): (&mut Renderer, &wgpu::Device),
        index: usize,
        measure: bool,
        (render_ms, finish_ms): (f64, f64),
    ) {
        self.measured.resize(index + 1, false);
        self.measured[index] = measure;
        if self.options.visibility && self.kind(index) == 0 {
            self.observed.push_back(index);
        }
        self.take_visibility(renderer, device);
        if !measure {
            return;
        }
        let kind = self.kind(index);
        self.triangles[kind].push(renderer.geometry_stats().total().1 as f64);
        if kind == 0 {
            self.views.push(renderer.diagnostic_view_times());
            self.render_ms.push(render_ms);
            self.finish_ms.push(finish_ms);
        }
    }

    /// Takes the visibility reports that have arrived, each its observed
    /// frame's.
    pub fn take_visibility(&mut self, renderer: &mut Renderer, device: &wgpu::Device) {
        for report in renderer.take_instance_visibility(device) {
            let index = self.observed.pop_front().expect("an observed frame");
            if self.measured[index] {
                self.reports.push(report);
            }
        }
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
        self.gpu[self.kind(index)].push((frame.total_ms, groups));
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
        if !self.reports.is_empty() {
            print(format!(
                "  camera visibility over {} observed frames, median / p95:",
                self.reports.len()
            ));
            let share = |pick: fn(&InstanceVisibilityReport) -> (f64, f64)| -> Vec<f64> {
                self.reports
                    .iter()
                    .map(|report| {
                        let (hidden, drawn) = pick(report);
                        if drawn > 0. {
                            100. * hidden / drawn
                        } else {
                            0.
                        }
                    })
                    .collect()
            };
            let reports = &self.reports;
            line(
                "opaque instances drawn",
                &reports
                    .iter()
                    .map(|r| r.drawn_instances as f64)
                    .collect::<Vec<_>>(),
                "",
            );
            line(
                "of them hidden",
                &share(|r| (r.hidden_instances as f64, r.drawn_instances as f64)),
                "%",
            );
            line(
                "opaque triangles drawn",
                &reports
                    .iter()
                    .map(|r| r.drawn_triangles as f64)
                    .collect::<Vec<_>>(),
                "",
            );
            line(
                "of them hidden",
                &share(|r| (r.hidden_triangles as f64, r.drawn_triangles as f64)),
                "%",
            );
        }
        if self.options.visibility {
            for (kind, triangles) in KINDS.iter().zip(&self.triangles) {
                line(&format!("camera triangles, {kind}"), triangles, "");
            }
        }
        print(format!(
            "  CPU per view over {} frames drawing every instance, median / p95:",
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
        if self.gpu.iter().all(Vec::is_empty) {
            return out.into_inner();
        }
        let kinds = if self.options.visibility { 2 } else { 1 };
        let mut names: Vec<&str> = Vec::new();
        for (_, groups) in self.gpu.iter().flatten() {
            for name in groups.keys() {
                if !names.contains(name) {
                    names.push(name);
                }
            }
        }
        let header: Vec<String> = KINDS[..kinds]
            .iter()
            .zip(&self.gpu)
            .map(|(kind, frames)| format!("{kind} ({} frames)", frames.len()))
            .collect();
        print(format!(
            "  GPU per pass group, median / p95 ms: {}",
            header.join("; ")
        ));
        let row = |label: &str, pick: &dyn Fn(&GpuFrame) -> f64| {
            let cells: Vec<String> = self.gpu[..kinds]
                .iter()
                .map(|frames| {
                    let (median, p95) = median_p95(&frames.iter().map(pick).collect::<Vec<_>>());
                    format!("{median:8.3} / {p95:8.3}")
                })
                .collect();
            print(format!("  {label:<40} {}", cells.join("   ")));
        };
        let sum = |groups: &BTreeMap<&str, f64>, of: &dyn Fn(&str) -> bool| -> f64 {
            groups
                .iter()
                .filter(|(name, _)| of(name))
                .map(|(_, ms)| ms)
                .sum()
        };
        row("frame", &|(total, _)| *total);
        row("frame less instance visibility", &|(total, groups)| {
            total - sum(groups, &|name| name == VISIBILITY)
        });
        row("opaque stage (sky, geometry, lighting)", &|(_, groups)| {
            sum(groups, &|name| OPAQUE.contains(&name))
        });
        row("directional shadow cascades", &|(_, groups)| {
            sum(groups, &|name| {
                name.starts_with("directional shadow cascade")
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
