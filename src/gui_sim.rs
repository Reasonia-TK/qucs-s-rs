use super::{App, Tool};
use crate::diagram::{self, DiagramKind, DiagramSpec};
use crate::doc::Doc;
use crate::schematic::Schematic;
use crate::sim::{self, Plot, SimResult};
use egui::{Context, Ui};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

struct RunSource {
    path: PathBuf,
    schematic: Schematic,
    state: SourceState,
}

#[derive(Debug, PartialEq)]
enum SourceState {
    Saved,
    Unsaved,
    BeforeExternalChange,
}

impl RunSource {
    fn matches(&self, doc: &Doc) -> bool {
        doc.path.as_ref() == Some(&self.path) && doc.sch == self.schematic
    }
}

pub(super) struct SimulationJob {
    tab_id: u64,
    number: u64,
    source: RunSource,
    receiver: Receiver<Result<SimResult, String>>,
}

struct Run {
    number: u64,
    source: RunSource,
    result: SimResult,
}

struct SimulationInput(PathBuf);

impl SimulationInput {
    fn new(source: &RunSource) -> Result<Self, String> {
        // Keep the snapshot beside the project so relative subcircuits and models resolve.
        // A unique name lets us capture the exact editor contents even when the file changes.
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let kind = format!("gui-{}-{since_epoch}-{serial}", std::process::id());
        let path = sim::unsaved_copy_path(&source.path, &kind)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let input = Self(path);
        let written = file.write_all(source.schematic.serialize().as_bytes());
        drop(file);
        written.map_err(|e| format!("{}: {e}", input.0.display()))?;
        Ok(input)
    }
}

impl Drop for SimulationInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

enum Blocker {
    SaveRequired,
    NoAnalysis,
    MissingNgspice(String),
    MissingQucs(String),
    Snapshot(String),
    Failed(String),
}

impl Blocker {
    fn from_error(error: String) -> Self {
        if error.contains("schematic contains no simulation") {
            Self::NoAnalysis
        } else if error.contains("ngspice not found") || error.contains("cannot run ngspice") {
            Self::MissingNgspice(error)
        } else if error.contains("Install Qucs-S") || error.contains("qucs-s not found") {
            Self::MissingQucs(error)
        } else {
            Self::Failed(error)
        }
    }

    fn headline(&self) -> &str {
        match self {
            Self::SaveRequired => "この回路図には保存先がありません",
            Self::NoAnalysis => "有効な解析設定がありません",
            Self::MissingNgspice(_) => "ngspice を起動できません",
            Self::MissingQucs(_) => "内蔵ネットリストで扱えない部品または解析があります",
            Self::Snapshot(_) => "解析用の回路図を作成できません",
            Self::Failed(_) => "シミュレーションを完了できませんでした",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Self::SaveRequired | Self::NoAnalysis => None,
            Self::MissingNgspice(s)
            | Self::MissingQucs(s)
            | Self::Snapshot(s)
            | Self::Failed(s) => Some(s),
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct AnalysisKey {
    name: String,
    kind: String,
    sweep: bool,
}

impl AnalysisKey {
    fn of(plot: &Plot) -> Self {
        Self {
            name: if plot.sweep.is_some() {
                &plot.base
            } else {
                &plot.name
            }
            .clone(),
            kind: plot.kind.clone(),
            sweep: plot.sweep.is_some(),
        }
    }

    fn label(&self) -> String {
        let kind = match self.kind.as_str() {
            "ac" => "AC",
            "tran" => "過渡",
            "dc" => "DC",
            "op" => "動作点",
            "noise" => "ノイズ",
            other => other,
        };
        let sweep = if self.sweep { "・スイープ" } else { "" };
        format!("{}（{kind}{sweep}）", self.name)
    }

    fn plots<'a>(&self, result: &'a SimResult) -> Vec<&'a Plot> {
        result
            .plots
            .iter()
            .filter(|plot| Self::of(plot) == *self)
            .collect()
    }
}

fn signal_names(plot: &Plot) -> impl Iterator<Item = &str> {
    plot.vectors
        .iter()
        .skip(usize::from(plot.scale().is_some()))
        .map(|v| v.name.as_str())
}

fn default_signals(plot: &Plot) -> Vec<String> {
    let voltages: Vec<String> = signal_names(plot)
        .filter(|name| name.starts_with("v("))
        .take(4)
        .map(str::to_owned)
        .collect();
    if voltages.is_empty() {
        signal_names(plot).take(4).map(str::to_owned).collect()
    } else {
        voltages
    }
}

struct Chart {
    width: u32,
    height: u32,
    output: Result<RenderedChart, String>,
}

struct RenderedChart {
    texture: egui::TextureHandle,
    plot_area: Option<(f64, f64, f64, f64, f64, f64, bool)>,
    markers: serde_json::Value,
}

pub(super) struct SimulationView {
    pub(super) queued: bool,
    next_run: u64,
    current: Option<Run>,
    previous: Option<Run>,
    blocker: Option<Blocker>,
    open: bool,
    docked: bool,
    compare: bool,
    analysis: Option<AnalysisKey>,
    signals: HashMap<AnalysisKey, Vec<String>>,
    diagram: DiagramKind,
    markers: Vec<f64>,
    chart: Option<Chart>,
}

impl Default for SimulationView {
    fn default() -> Self {
        Self {
            queued: false,
            next_run: 1,
            current: None,
            previous: None,
            blocker: None,
            open: false,
            docked: true,
            compare: false,
            analysis: None,
            signals: HashMap::new(),
            diagram: DiagramKind::Rect,
            markers: Vec::new(),
            chart: None,
        }
    }
}

impl SimulationView {
    pub(super) fn invalidate_chart(&mut self) {
        self.chart = None;
    }

    fn analyses(&self) -> Vec<AnalysisKey> {
        let mut analyses = Vec::new();
        if let Some(current) = &self.current {
            for plot in &current.result.plots {
                let key = AnalysisKey::of(plot);
                if !analyses.contains(&key) {
                    analyses.push(key);
                }
            }
        }
        analyses
    }

    fn accept(&mut self, number: u64, source: RunSource, result: Result<SimResult, String>) {
        match result {
            Ok(result) if !result.plots.is_empty() => {
                let partial = (!result.ok).then(|| Blocker::Failed(result.errors.join("\n")));
                // Keep the last distinct circuit for comparison, even when rerunning the same values.
                if self.current.as_ref().is_some_and(|run| {
                    run.source.path != source.path || run.source.schematic != source.schematic
                }) {
                    self.previous = self.current.take();
                }
                self.current = Some(Run {
                    number,
                    source,
                    result,
                });
                let analyses = self.analyses();
                if self
                    .analysis
                    .as_ref()
                    .is_none_or(|selected| !analyses.contains(selected))
                {
                    self.analysis = analyses.into_iter().next();
                    self.markers.clear();
                }
                self.blocker = partial;
                self.open = true;
                self.invalidate_chart();
            }
            Ok(result) => {
                let messages = if result.errors.is_empty() {
                    result.log
                } else {
                    result.errors.join("\n")
                };
                self.blocker = Some(Blocker::Failed(messages));
            }
            Err(error) => self.blocker = Some(Blocker::from_error(error)),
        }
    }

    fn stale(&self, doc: &Doc) -> bool {
        self.current
            .as_ref()
            .is_some_and(|run| !run.source.matches(doc))
    }
}

impl App {
    pub(super) fn run_sim(&mut self) {
        // Start after the full UI pass so a focused property field can commit that frame.
        self.simulation.queued = true;
    }

    pub(super) fn simulation_running(&self) -> bool {
        self.sim_job
            .as_ref()
            .is_some_and(|job| job.tab_id == self.tab_id)
    }

    pub(super) fn start_requested_simulation(&mut self, ctx: &Context) {
        if !self.simulation.queued || self.sim_job.is_some() {
            return;
        }
        self.simulation.queued = false;
        self.simulation.blocker = None;
        if self.finish_buffered_property_edit() {
            self.auto_save_current();
        }
        let Some(path) = self.doc.path.clone() else {
            self.simulation.blocker = Some(Blocker::SaveRequired);
            ctx.request_repaint();
            return;
        };
        // Qucs stores analysis settings as active dot-prefixed components, including
        // analysis types that the built-in netlister delegates to Qucs-S.
        if !self
            .doc
            .sch
            .components
            .iter()
            .any(|c| c.active & 3 == 1 && c.typ.starts_with('.'))
        {
            self.simulation.blocker = Some(Blocker::NoAnalysis);
            ctx.request_repaint();
            return;
        }
        if let Err(error) = sim::ngspice_path() {
            self.simulation.blocker = Some(Blocker::MissingNgspice(error));
            ctx.request_repaint();
            return;
        }
        let state = if self.doc.dirty {
            SourceState::Unsaved
        } else if self.external_change {
            SourceState::BeforeExternalChange
        } else {
            SourceState::Saved
        };
        let source = RunSource {
            path,
            schematic: self.doc.sch.clone(),
            state,
        };
        let input = match SimulationInput::new(&source) {
            Ok(input) => input,
            Err(error) => {
                self.simulation.blocker = Some(Blocker::Snapshot(error));
                ctx.request_repaint();
                return;
            }
        };
        let (sender, receiver) = channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = sim::simulate(&input.0, 300);
            drop(input);
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        let number = self.simulation.next_run;
        self.simulation.next_run += 1;
        self.sim_job = Some(SimulationJob {
            tab_id: self.tab_id,
            number,
            source,
            receiver,
        });
        self.status = format!("シミュレーション 実行 {number} を計算中…");
        ctx.request_repaint();
    }

    pub(super) fn poll_simulation(&mut self) {
        let Some(job) = &self.sim_job else { return };
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("計算処理との接続が切れました。".into()),
        };
        let Some(job) = self.sim_job.take() else {
            return;
        };
        if job.tab_id == self.tab_id {
            self.simulation.accept(job.number, job.source, result);
            self.status = if let Some(blocker) = &self.simulation.blocker {
                blocker.headline().into()
            } else if let Some(run) = &self.simulation.current {
                format!(
                    "シミュレーション 実行 {} 完了（{:.2} 秒）",
                    run.number, run.result.elapsed
                )
            } else {
                "シミュレーション完了".into()
            };
        } else {
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .flatten()
                .find(|tab| tab.tab_id == job.tab_id)
            {
                tab.simulation.accept(job.number, job.source, result);
            }
            if self.status == format!("シミュレーション 実行 {} を計算中…", job.number)
            {
                self.status = "バックグラウンドのシミュレーションが終了しました".into();
            }
        }
    }

    pub(super) fn simulation_status_button(&mut self, ui: &mut Ui) {
        if self.simulation.queued {
            ui.small("シミュレーション待機中");
        }
        if self.simulation.current.is_some() {
            let caption = if self.simulation.stale(&self.doc) {
                "前回の結果"
            } else {
                "結果"
            };
            if ui.small_button(caption).clicked() {
                self.simulation.open = true;
            }
        }
    }

    pub(super) fn simulation_notice(&mut self, ctx: &Context) {
        let Some(blocker) = self.simulation.blocker.as_ref() else {
            return;
        };
        let mut close = false;
        let mut retry = false;
        let mut save = false;
        let mut about = false;
        let mut check = false;
        let mut place = None;
        egui::TopBottomPanel::top("simulation_notice").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(self.pal().warn, blocker.headline());
                match blocker {
                    Blocker::SaveRequired => {
                        save = ui.button("名前を付けて保存して実行").clicked();
                    }
                    Blocker::NoAnalysis => {
                        ui.label("解析を選び、回路図をクリックして配置してください。");
                        for (kind, label) in [(".AC", "AC 解析"), (".TR", "過渡解析"), (".DC", "DC 動作点")] {
                            if ui.button(label).clicked() {
                                place = Some((kind, label));
                            }
                        }
                    }
                    Blocker::MissingNgspice(_) => {
                        ui.hyperlink_to("ngspice の入手先", "https://ngspice.sourceforge.io/");
                        ui.label("PATH または NGSPICE_PATH を設定し、アプリを再起動してください。");
                        about = ui.button("検出状況").clicked();
                    }
                    Blocker::MissingQucs(_) => {
                        ui.hyperlink_to("Qucs-S の入手先", "https://ra3xdh.github.io/");
                        ui.label("対応する部品に置き換えるか、Qucs-S を導入して自動切り替えを有効にしてください。");
                        about = ui.button("検出状況").clicked();
                    }
                    Blocker::Snapshot(_) => {
                        ui.label("書き込み可能なフォルダーに保存してください。");
                        save = ui.button("別の場所に保存して実行").clicked();
                    }
                    Blocker::Failed(_) => {
                        check = ui.button("回路をチェック").clicked();
                        retry = ui.button("再実行").clicked();
                    }
                }
                close = ui.button("閉じる").clicked();
            });
            if let Some(detail) = blocker.detail().filter(|s| !s.is_empty()) {
                ui.collapsing("エラーの詳細", |ui| {
                    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        ui.add(egui::Label::new(detail).selectable(true));
                    });
                });
            }
            if matches!(blocker, Blocker::MissingQucs(_)) {
                ui.small("QUCS_RS_NETLISTER=auto で自動切り替えます。標準の場所以外にインストールした場合は QUCS_S_PATH を設定してください。");
            }
        });
        if close {
            self.simulation.blocker = None;
        }
        if let Some((kind, label)) = place {
            self.tool = Tool::Place(kind.into());
            self.place_rot = crate::library::new_component(kind, "X", 0, 0)
                .map(|c| c.rotate)
                .unwrap_or(0);
            self.simulation.blocker = None;
            self.status = format!("回路図をクリックして{label}を配置してください");
        }
        if about {
            self.about_open = true;
        }
        if check {
            self.show_issue_panel = true;
            self.show_issues = true;
            self.refresh_issues();
            self.gen += 1;
        }
        if retry || (save && self.save_as()) {
            self.run_sim();
        }
    }

    pub(super) fn simulation_panel(&mut self, ctx: &Context) -> bool {
        if self.simulation.open && self.simulation.docked {
            let height = ctx.content_rect().height();
            egui::TopBottomPanel::bottom("simulation_dock")
                .default_height(355.0)
                .height_range(180.0..=(height * 0.7).max(180.0))
                .resizable(true)
                .show(ctx, |ui| {
                    ui.set_min_height(ui.available_height());
                    self.simulation_ui(ui);
                });
            true
        } else {
            false
        }
    }

    pub(super) fn simulation_window(&mut self, ctx: &Context) {
        if self.simulation.open && !self.simulation.docked {
            let mut open = true;
            egui::Window::new("シミュレーション結果")
                .open(&mut open)
                .default_size([940.0, 610.0])
                .show(ctx, |ui| self.simulation_ui(ui));
            self.simulation.open &= open;
        }
    }

    fn simulation_ui(&mut self, ui: &mut Ui) {
        let running = self.simulation_running();
        let stale = self.simulation.stale(&self.doc);
        let mut rerun = false;
        self.simulation
            .controls(ui, &self.doc, stale, running, &mut rerun);
        self.simulation.waveform(ui);
        if rerun {
            self.run_sim();
        }
    }
}

impl SimulationView {
    fn controls(&mut self, ui: &mut Ui, doc: &Doc, stale: bool, running: bool, rerun: &mut bool) {
        let Some(run) = &self.current else { return };
        ui.horizontal_wrapped(|ui| {
            if self.docked {
                ui.strong("シミュレーション結果");
            }
            let saved = match run.source.state {
                SourceState::Saved => "保存済みの回路図",
                SourceState::Unsaved => "未保存の編集内容",
                SourceState::BeforeExternalChange => "外部変更の取り込み前",
            };
            let title = format!("実行 {} · {saved}", run.number);
            ui.label(title).on_hover_text(format!(
                "{}\n実行時の回路図と、編集中の回路図を比較して更新状態を表示します。",
                run.source.path.display()
            ));
            if stale {
                let label = if running || self.queued {
                    "編集前の回路図の結果"
                } else {
                    "回路変更後、未実行"
                };
                ui.colored_label(ui.visuals().warn_fg_color, label);
            } else if !run.result.ok {
                ui.colored_label(ui.visuals().warn_fg_color, "一部の解析結果");
            }
            *rerun = ui
                .add_enabled(!running && !self.queued, egui::Button::new("再実行"))
                .clicked();
            if running || self.queued {
                ui.spinner();
                ui.label(if self.queued {
                    "再実行を待機中"
                } else {
                    "計算中…"
                });
            }
            if ui
                .small_button(if self.docked {
                    "別ウィンドウに表示"
                } else {
                    "回路図の下に表示"
                })
                .clicked()
            {
                self.docked = !self.docked;
            }
            if self.docked && ui.small_button("閉じる").clicked() {
                self.open = false;
            }
        });
        if stale || self.previous.is_some() {
            ui.collapsing("回路図の変更点", |ui| {
                let (before, after, relation) = if stale {
                    (
                        &run.source.schematic,
                        &doc.sch,
                        format!("実行 {} → 現在の回路図", run.number),
                    )
                } else if let Some(previous) = &self.previous {
                    (
                        &previous.source.schematic,
                        &run.source.schematic,
                        format!("実行 {} → 実行 {}", previous.number, run.number),
                    )
                } else {
                    return;
                };
                ui.small(relation);
                let differences = circuit_differences(before, after);
                if differences.is_empty() {
                    ui.label("ファイル名または回路図設定が変更されました。");
                } else {
                    for difference in differences {
                        ui.label(difference);
                    }
                }
            });
        }
        if !run.result.errors.is_empty() {
            ui.collapsing(
                format!("計算時のメッセージ（{} 件）", run.result.errors.len()),
                |ui| {
                    for error in &run.result.errors {
                        ui.label(error);
                    }
                },
            );
        }
    }

    fn waveform(&mut self, ui: &mut Ui) {
        let entries = self.analyses();
        let Some(mut analysis) = self.analysis.clone() else {
            return;
        };
        let Some(run) = &self.current else { return };
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("analysis")
                .selected_text(analysis.label())
                .show_ui(ui, |ui| {
                    for entry in &entries {
                        changed |= ui
                            .selectable_value(&mut analysis, entry.clone(), entry.label())
                            .changed();
                    }
                })
                .response
                .on_hover_text("表示する解析");
            let is_op = analysis.kind == "op";
            for (kind, label) in [
                (DiagramKind::Rect, "直交"),
                (DiagramKind::Polar, "極座標"),
                (DiagramKind::Smith, "スミス"),
                (DiagramKind::Table, "表"),
            ] {
                let selected = if is_op {
                    kind == DiagramKind::Table
                } else {
                    self.diagram == kind
                };
                if ui
                    .add_enabled(!is_op, egui::Button::selectable(selected, label))
                    .clicked()
                {
                    self.diagram = kind;
                    changed = true;
                }
            }
            if ui
                .add_enabled(!self.markers.is_empty(), egui::Button::new("マーカー消去"))
                .clicked()
            {
                self.markers.clear();
                changed = true;
            }
        });
        if self.analysis.as_ref() != Some(&analysis) {
            self.analysis = Some(analysis.clone());
            self.markers.clear();
        }
        let mut plots = analysis.plots(&run.result);
        let Some(plot) = plots.first() else { return };
        let selected = self
            .signals
            .entry(analysis.clone())
            .or_insert_with(|| default_signals(plot));
        let kind = if analysis.kind == "op" {
            DiagramKind::Table
        } else {
            self.diagram.clone()
        };
        let supported = |name: &str| {
            plot.vector(name).is_some_and(|v| {
                !matches!(kind, DiagramKind::Polar | DiagramKind::Smith) || v.is_complex()
            })
        };
        let available: Vec<&str> = signal_names(plot).filter(|name| supported(name)).collect();
        if !selected.is_empty()
            && !selected
                .iter()
                .any(|name| signal_names(plot).any(|candidate| candidate == name))
        {
            // The selected signals may have been removed or renamed by the latest run.
            *selected = default_signals(plot);
            changed = true;
        }
        ui.horizontal_wrapped(|ui| {
            for name in &available {
                let mut on = selected.iter().any(|signal| signal == name);
                if ui.checkbox(&mut on, *name).changed() {
                    if on {
                        selected.push((*name).into());
                    } else {
                        selected.retain(|signal| signal != name);
                    }
                    changed = true;
                }
            }
        });
        let vectors: Vec<String> = available
            .iter()
            .filter(|name| selected.iter().any(|signal| signal == *name))
            .map(|name| (*name).into())
            .collect();
        let previous = self
            .previous
            .as_ref()
            .map(|prior| (prior.number, analysis.plots(&prior.result)));
        let mut previous_start = None;
        if let Some((number, prior_plots)) = previous {
            let compatible = kind == DiagramKind::Rect
                && !vectors.is_empty()
                && matching_plots(&plots, &prior_plots, &vectors);
            if ui
                .add_enabled(
                    compatible,
                    egui::Checkbox::new(&mut self.compare, format!("実行 {number} の波形を重ねる")),
                )
                .on_disabled_hover_text("同じ解析・信号・スイープの直交グラフを比較できます")
                .changed()
            {
                changed = true;
            }
            if compatible && self.compare {
                previous_start = Some(plots.len());
                plots.extend(prior_plots);
                ui.small(format!(
                    "凡例の current は実行 {}、previous は実行 {number}",
                    run.number
                ));
            }
        }
        if vectors.is_empty() {
            ui.label(if available.is_empty() {
                "このグラフ形式で表示できる信号がありません。"
            } else {
                "表示する信号を選択してください。"
            });
            if changed {
                self.chart = None;
            }
            return;
        }
        ui.separator();
        let width = ui.available_width().clamp(240.0, 1600.0) as u32;
        let height = (ui.available_height() - 36.0).clamp(235.0, 760.0) as u32;
        let spec = DiagramSpec {
            kind,
            vectors,
            markers: self.markers.clone(),
            previous_start,
            ..Default::default()
        };
        if changed
            || self
                .chart
                .as_ref()
                .is_none_or(|chart| chart.width != width || chart.height != height)
        {
            let output = render_chart(ui.ctx(), &plots, &spec, width, height);
            self.chart = Some(Chart {
                width,
                height,
                output,
            });
        }
        let Some(chart) = &self.chart else { return };
        match &chart.output {
            Ok(chart) => {
                let mut marker = None;
                egui::ScrollArea::vertical()
                    .id_salt("simulation_chart")
                    .auto_shrink([false, self.docked])
                    .show(ui, |ui| {
                        let size = chart.texture.size_vec2();
                        let scale = (ui.available_width() / size.x).min(1.0);
                        let image = ui.add(
                            egui::Image::new((chart.texture.id(), size * scale))
                                .sense(egui::Sense::click()),
                        );
                        if let Some((left, top, width, _, start, end, log)) = chart.plot_area {
                            if let Some(pos) =
                                image.interact_pointer_pos().filter(|_| image.clicked())
                            {
                                let x = f64::from((pos.x - image.rect.min.x) / scale);
                                let y = f64::from((pos.y - image.rect.min.y) / scale);
                                if (left..=left + width).contains(&x) && y >= top {
                                    let f = (x - left) / width;
                                    marker = Some(if log {
                                        (start.ln() + f * (end.ln() - start.ln())).exp()
                                    } else {
                                        start + f * (end - start)
                                    });
                                }
                            }
                            ui.small("グラフ上をクリックすると測定値を表示します。");
                        }
                        show_markers(ui, &chart.markers);
                    });
                if let Some(x) = marker {
                    self.markers.push(x);
                    self.chart = None;
                }
            }
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }
    }
}

fn matching_plots(current: &[&Plot], previous: &[&Plot], signals: &[String]) -> bool {
    !previous.is_empty()
        && current.len() == previous.len()
        && current.iter().zip(previous).all(|(new, old)| {
            new.sweep == old.sweep
                && new.scale().map(|v| &v.unit) == old.scale().map(|v| &v.unit)
                && signals.iter().all(|name| {
                    new.vector(name)
                        .zip(old.vector(name))
                        .is_some_and(|(a, b)| a.unit == b.unit && a.is_complex() == b.is_complex())
                })
        })
}

fn render_chart(
    ctx: &Context,
    plots: &[&Plot],
    spec: &DiagramSpec,
    width: u32,
    height: u32,
) -> Result<RenderedChart, String> {
    let diagram = diagram::diagram_svg(plots, spec, f64::from(width), f64::from(height))?;
    let (w, h, rgba) = crate::render::svg_to_rgba(&diagram.svg)?;
    let image = egui::ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        &crate::theme::pal(ctx).map_image(&rgba),
    );
    Ok(RenderedChart {
        texture: ctx.load_texture("simulation_plot", image, egui::TextureOptions::LINEAR),
        plot_area: diagram.plot_area,
        markers: diagram.markers,
    })
}

fn show_markers(ui: &mut Ui, markers: &serde_json::Value) {
    let Some(rows) = markers.as_array().filter(|rows| !rows.is_empty()) else {
        return;
    };
    egui::Grid::new("simulation_markers")
        .striped(true)
        .show(ui, |ui| {
            for row in rows {
                let x = crate::plot::eng(row.get("x").and_then(|x| x.as_f64()).unwrap_or_default());
                let signal = row
                    .get("label")
                    .or_else(|| row.get("vector"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let value = &row["value"];
                let shown = if let Some(value) = value.as_f64() {
                    crate::plot::eng(value)
                } else if let (Some(db), Some(phase)) =
                    (value["db"].as_f64(), value["phase_deg"].as_f64())
                {
                    format!("{db:.2} dB, {phase:.1}°")
                } else {
                    "—".into()
                };
                ui.label(x);
                ui.label(signal);
                ui.label(shown);
                ui.end_row();
            }
        });
}

fn circuit_differences(before: &Schematic, after: &Schematic) -> Vec<String> {
    let mut differences = Vec::new();
    let next_parts: HashMap<_, _> = after
        .components
        .iter()
        .map(|c| ((c.name.as_str(), c.typ.as_str()), c))
        .collect();
    for part in &before.components {
        let Some(next) = next_parts.get(&(part.name.as_str(), part.typ.as_str())) else {
            continue;
        };
        for (i, (old, new)) in part.props.iter().zip(&next.props).enumerate() {
            if old.value != new.value {
                differences.push(format!(
                    "{} · {}　{} → {}",
                    part.name,
                    crate::library::prop_name(part, i),
                    old.value,
                    new.value
                ));
                if differences.len() == 6 {
                    differences.push("ほかの変更は回路図で確認できます。".into());
                    return differences;
                }
            }
        }
    }
    if before.wires != after.wires {
        differences.push("配線が変更されました。".into());
    }
    if differences.is_empty() && before.components != after.components {
        differences.push("部品または解析設定が変更されました。".into());
    }
    differences
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Vector;

    fn circuit() -> Schematic {
        Schematic::parse(include_str!("../examples/inverting_amp.sch")).unwrap()
    }

    fn plot(name: &str, kind: &str) -> Plot {
        let vector = |name: &str, unit: &str| Vector {
            name: name.into(),
            unit: unit.into(),
            re: vec![0.0, 1.0],
            im: None,
        };
        Plot {
            name: name.into(),
            base: name.into(),
            kind: kind.into(),
            title: String::new(),
            vectors: vec![vector("time", "s"), vector("v(out)", "V")],
            sweep: None,
        }
    }

    fn successful_result() -> SimResult {
        SimResult {
            ok: true,
            plots: vec![plot("ac1", "ac"), plot("tr1", "tran")],
            ..SimResult::default()
        }
    }

    #[test]
    fn result_tracks_the_input_and_keeps_the_previous_distinct_run() {
        let path = PathBuf::from("test.sch");
        let mut doc = Doc::new(circuit(), Some(path.clone()));
        let original = doc.sch.clone();
        let mut view = SimulationView::default();
        let source = |schematic, state| RunSource {
            path: path.clone(),
            schematic,
            state,
        };

        view.accept(
            1,
            source(original.clone(), SourceState::Saved),
            Ok(successful_result()),
        );
        let transient = AnalysisKey::of(&successful_result().plots[1]);
        view.analysis = Some(transient.clone());
        view.signals
            .insert(transient.clone(), vec!["v(out)".into()]);
        assert!(!view.stale(&doc));
        doc.sch.component_mut("R1").unwrap().props[0].value = "20k".into();
        assert!(view.stale(&doc));
        view.accept(
            2,
            source(doc.sch.clone(), SourceState::Unsaved),
            Ok(successful_result()),
        );
        assert!(!view.stale(&doc));
        assert!(view.analysis.as_ref() == Some(&transient));
        assert_eq!(view.signals[&transient], ["v(out)"]);
        assert_eq!(view.previous.as_ref().unwrap().number, 1);
        assert_eq!(
            view.current.as_ref().unwrap().source.state,
            SourceState::Unsaved
        );

        view.accept(
            3,
            source(doc.sch.clone(), SourceState::Unsaved),
            Ok(successful_result()),
        );
        assert_eq!(view.previous.as_ref().unwrap().number, 1);
        view.accept(
            4,
            source(doc.sch.clone(), SourceState::Unsaved),
            Err("calculation failed".into()),
        );
        assert_eq!(view.current.as_ref().unwrap().number, 3);
        doc.sch = original;
        assert!(view.stale(&doc));
    }

    #[test]
    fn simulation_snapshot_is_an_independent_sibling_and_is_removed() {
        let path =
            std::env::temp_dir().join(format!("qucs-result-test-{}.sch", std::process::id()));
        let mut current = circuit();
        let source = RunSource {
            path: path.clone(),
            schematic: current.clone(),
            state: SourceState::Saved,
        };
        let input = SimulationInput::new(&source).unwrap();
        let snapshot = input.0.clone();
        assert_eq!(snapshot.parent(), path.parent());
        assert_ne!(snapshot, path);
        current.component_mut("R1").unwrap().props[0].value = "20k".into();
        assert_eq!(Schematic::load(&snapshot).unwrap(), source.schematic);
        assert_ne!(Schematic::load(&snapshot).unwrap(), current);
        drop(input);
        assert!(!snapshot.exists());
    }

    #[test]
    fn previous_run_comparison_requires_compatible_signals() {
        let mut old = plot("tr1", "tran");
        let new = old.clone();
        assert!(matching_plots(&[&new], &[&old], &["v(out)".into()]));
        old.vectors[1].unit = "A".into();
        assert!(!matching_plots(&[&new], &[&old], &["v(out)".into()]));
        old.vectors.pop();
        assert!(!matching_plots(&[&new], &[&old], &["v(out)".into()]));
    }
}
