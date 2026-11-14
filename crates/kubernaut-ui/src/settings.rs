//! Settings: the few preferences that change how the app behaves, saved as soon
//! as they change.
//!
//! Only settings that already do something are listed. Each cluster can be
//! given a name, a group and a colour, so production looks different from
//! staging, marked protected so changes from this app are refused outright, and
//! given a default namespace.

use std::sync::Arc;

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore, helm_ops,
    preferences::Preferences,
    updates::{self, Release},
};
use tokio::runtime::Handle;

use crate::ui::{Emphasis, button, chip, segment, segmented_bar, toast_error};

/// How many lines a log view asks for when it opens.
const TAIL_CHOICES: [i64; 4] = [200, 500, 1000, 5000];

/// A namespace as typed: blank clears it, anything else must be one Kubernetes
/// accepts.
fn namespace_edit(value: &str) -> Result<Option<String>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    helm_ops::check_namespace(value)?;
    Ok(Some(value.to_string()))
}

/// A text box that saves when Enter is pressed or focus leaves it.
struct TextField {
    input: Entity<InputState>,
    /// What is saved, so leaving the field untouched does not write the file.
    saved: Option<String>,
    _input: Subscription,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Namespace,
    Display,
    Workspace,
}

/// Colours offered for a cluster, as the hex the preferences file stores. Red
/// first: the cheapest guard against acting on production by mistake.
const COLOURS: [(&str, &str); 8] = [
    ("#ef4444", "Red"),
    ("#f97316", "Orange"),
    ("#eab308", "Yellow"),
    ("#22c55e", "Green"),
    ("#06b6d4", "Cyan"),
    ("#3b82f6", "Blue"),
    ("#8b5cf6", "Violet"),
    ("#ec4899", "Pink"),
];

/// Longest name or workspace worth showing in a rail tile and a toolbar.
const MAX_LABEL: usize = 40;

/// A name or workspace as typed: blank clears it, and a very long one is
/// refused rather than cut, so nothing is saved that was not what was typed.
fn label_edit(value: &str) -> Result<Option<String>, &'static str> {
    let value = value.trim();
    if value.chars().count() > MAX_LABEL {
        return Err("Keep it under 40 characters.");
    }
    Ok((!value.is_empty()).then(|| value.to_string()))
}

struct ClusterRow {
    name: SharedString,
    protected: bool,
    namespace: TextField,
    display: TextField,
    workspace: TextField,
    colour: Option<String>,
    error: Option<SharedString>,
}

/// Tells the app the rail must be rebuilt: a name, colour or group changed.
pub enum SettingsEvent {
    ClustersChanged,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

/// Where the update check stands.
enum UpdateState {
    Idle,
    Checking,
    /// Checked, and the running version is the latest.
    Current,
    Available(Release),
    /// Downloading, verifying or handing over to the installer.
    Working(&'static str),
    Failed(SharedString),
}

pub struct SettingsView {
    core: Arc<AppCore>,
    runtime: Handle,
    tail: i64,
    check_on_startup: bool,
    update: UpdateState,
    /// The cluster whose card is marked, because it was sent here to be edited.
    highlighted: Option<usize>,
    rows: Vec<ClusterRow>,
    path: Option<SharedString>,
}

impl SettingsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = core.preferences();
        let rows = core
            .clusters
            .contexts()
            .into_iter()
            .enumerate()
            .map(|(index, context)| {
                let profile = preferences.profile(&context.name);
                let mut field = |which: Field, saved: Option<String>, placeholder: &'static str| {
                    let input = cx.new(|cx| {
                        let mut state = InputState::new(window, cx).placeholder(placeholder);
                        if let Some(text) = &saved {
                            state.set_value(text.clone(), window, cx);
                        }
                        state
                    });
                    let subscription = cx.subscribe_in(
                        &input,
                        window,
                        move |this, _, event: &InputEvent, window, cx| {
                            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                                this.commit(index, which, window, cx);
                            }
                        },
                    );
                    TextField {
                        input,
                        saved,
                        _input: subscription,
                    }
                };
                let name: SharedString = context.name.clone().into();
                ClusterRow {
                    protected: preferences.is_protected(&context.name),
                    namespace: field(
                        Field::Namespace,
                        profile.default_namespace,
                        "All namespaces",
                    ),
                    display: field(Field::Display, profile.display_name, "Same as the context"),
                    workspace: field(Field::Workspace, profile.workspace, "None"),
                    colour: profile.colour,
                    name,
                    error: None,
                }
            })
            .collect();
        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            tail: preferences.log_tail_lines,
            check_on_startup: preferences.check_updates_on_startup,
            update: UpdateState::Idle,
            highlighted: None,
            rows,
            path: preferences.path().map(Into::into),
        }
    }

    /// Save one change off the UI thread, and say so if it could not be kept.
    fn persist(
        &self,
        what: &'static str,
        edit: impl FnOnce(&mut Preferences) + Send + 'static,
        rebuild_rail: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { core.update_preferences(edit) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |_, window, cx| match result {
                Ok(()) if rebuild_rail => cx.emit(SettingsEvent::ClustersChanged),
                Ok(()) => {}
                Err(reason) => toast_error(window, cx, format!("Could not save {what}"), reason),
            })
            .ok();
        })
        .detach();
    }

    fn set_tail(&mut self, lines: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.tail = lines;
        self.persist(
            "the log length",
            move |p| p.log_tail_lines = lines,
            false,
            window,
            cx,
        );
        cx.notify();
    }

    fn set_check_on_startup(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.check_on_startup = on;
        self.persist(
            "the update setting",
            move |p| p.check_updates_on_startup = on,
            false,
            window,
            cx,
        );
        cx.notify();
    }

    /// Mark a cluster's card and put the cursor in its name, which is what a
    /// person coming to edit it types first.
    pub fn show_cluster(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.rows.iter().position(|row| *row.name == *name) else {
            return;
        };
        self.highlighted = Some(index);
        if let Some(row) = self.rows.get(index) {
            row.display
                .input
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    pub fn check_for_update(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Working(_)) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();
        let task = self
            .runtime
            .spawn_blocking(|| updates::check(env!("CARGO_PKG_VERSION")));
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.update = match result {
                    Ok(Some(release)) => UpdateState::Available(release),
                    Ok(None) => UpdateState::Current,
                    Err(reason) => UpdateState::Failed(reason.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Download, verify and stage the release, then hand the swap to a helper
    /// and quit so it can run.
    fn install_update(&mut self, release: Release, cx: &mut Context<Self>) {
        self.update = UpdateState::Working("Downloading and checking the signature…");
        cx.notify();
        let task = self.runtime.spawn_blocking(move || -> Result<(), String> {
            let bundle = updates::current_bundle().ok_or(
                "this copy is not running from an app bundle, so it cannot replace itself",
            )?;
            let directory = updates::staging_directory()?;
            let outcome = (|| {
                let zip = updates::download(&release, &directory)?;
                let staged = updates::stage(&zip, &directory, &release.version)?;
                updates::install(&staged, &bundle)
            })();
            if outcome.is_err() {
                let _ = std::fs::remove_dir_all(&directory);
            }
            outcome
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| match result {
                Ok(()) => cx.quit(),
                Err(reason) => {
                    this.update = UpdateState::Failed(reason.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_protected(
        &mut self,
        index: usize,
        protected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if row.protected == protected {
            return;
        }
        row.protected = protected;
        let name = row.name.to_string();
        self.persist(
            "the protected list",
            move |p| p.set_protected(&name, protected),
            false,
            window,
            cx,
        );
        cx.notify();
    }

    fn commit(&mut self, index: usize, which: Field, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        let name = row.name.to_string();
        let field = match which {
            Field::Namespace => &mut row.namespace,
            Field::Display => &mut row.display,
            Field::Workspace => &mut row.workspace,
        };
        let typed = field.input.read(cx).value();
        let checked = match which {
            Field::Namespace => namespace_edit(&typed),
            Field::Display | Field::Workspace => label_edit(&typed),
        };
        let value = match checked {
            Ok(value) => value,
            Err(reason) => {
                row.error = Some(reason.into());
                cx.notify();
                return;
            }
        };
        row.error = None;
        if value != field.saved {
            field.saved = value.clone();
            match which {
                Field::Namespace => self.persist(
                    "the default namespace",
                    move |p| p.set_default_namespace(&name, value.as_deref()),
                    false,
                    window,
                    cx,
                ),
                Field::Display => self.persist(
                    "the name",
                    move |p| p.update_profile(&name, |profile| profile.display_name = value),
                    true,
                    window,
                    cx,
                ),
                Field::Workspace => self.persist(
                    "the group",
                    move |p| p.update_profile(&name, |profile| profile.workspace = value),
                    true,
                    window,
                    cx,
                ),
            }
        }
        cx.notify();
    }

    fn set_colour(
        &mut self,
        index: usize,
        colour: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        let colour = colour.map(str::to_string);
        if row.colour == colour {
            return;
        }
        row.colour = colour.clone();
        let name = row.name.to_string();
        self.persist(
            "the colour",
            move |p| p.update_profile(&name, |profile| profile.colour = colour),
            true,
            window,
            cx,
        );
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, danger, secondary, primary) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.danger,
                theme.secondary,
                theme.primary,
            )
        };
        let card = || {
            v_flex()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .overflow_hidden()
        };
        let heading = |text: &'static str| {
            div()
                .text_xs()
                .font_semibold()
                .text_color(muted)
                .child(text)
        };
        let line = |title: &'static str, note: &'static str, control: AnyElement| {
            h_flex()
                .px_4()
                .py_3()
                .gap_4()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(title))
                        .child(div().text_xs().text_color(muted).child(note)),
                )
                .child(control)
        };

        let tail = TAIL_CHOICES.iter().enumerate().fold(
            segmented_bar(cx.theme()),
            |bar, (index, lines)| {
                let lines = *lines;
                bar.child(
                    segment(
                        ("settings-tail", index),
                        lines.to_string(),
                        self.tail == lines,
                        cx.theme(),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_tail(lines, window, cx)),
                    ),
                )
            },
        );

        let startup = segmented_bar(cx.theme())
            .child(
                segment(
                    "settings-updates-on",
                    "On",
                    self.check_on_startup,
                    cx.theme(),
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.set_check_on_startup(true, window, cx)),
                ),
            )
            .child(
                segment(
                    "settings-updates-off",
                    "Off",
                    !self.check_on_startup,
                    cx.theme(),
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.set_check_on_startup(false, window, cx)),
                ),
            );
        let busy = matches!(self.update, UpdateState::Checking | UpdateState::Working(_));
        let (status, status_color): (SharedString, Hsla) = match &self.update {
            UpdateState::Idle => (
                format!("Version {}", env!("CARGO_PKG_VERSION")).into(),
                muted,
            ),
            UpdateState::Checking => ("Looking for a newer version…".into(), muted),
            UpdateState::Current => (
                format!("Version {} is the latest.", env!("CARGO_PKG_VERSION")).into(),
                muted,
            ),
            UpdateState::Available(release) => (
                format!("Version {} is available.", release.version).into(),
                cx.theme().success,
            ),
            UpdateState::Working(what) => ((*what).into(), muted),
            UpdateState::Failed(reason) => (reason.clone(), danger),
        };
        let mut update_actions = h_flex().gap_2().items_center().child(
            button(
                "settings-update-check",
                "Check now",
                !busy,
                Emphasis::Plain,
                cx.theme(),
            )
            .when(!busy, |b| {
                b.on_click(cx.listener(|this, _, _, cx| this.check_for_update(cx)))
            }),
        );
        if let UpdateState::Available(release) = &self.update {
            let page = release.page_url.clone();
            update_actions = update_actions.child(
                button(
                    "settings-update-notes",
                    "Release notes",
                    true,
                    Emphasis::Plain,
                    cx.theme(),
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&page))),
            );
            if release.installable() {
                let release = release.clone();
                update_actions = update_actions.child(
                    button(
                        "settings-update-install",
                        "Install and restart",
                        true,
                        Emphasis::Primary,
                        cx.theme(),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.install_update(release.clone(), cx)),
                    ),
                );
            }
        }
        let signed_note = match &self.update {
            UpdateState::Available(release) if !release.installable() => Some(
                "This build cannot install updates itself: download the new version from the \
                 release notes page.",
            ),
            _ => None,
        };

        let mut clusters = card();
        let count = self.rows.len();
        for index in 0..count {
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            let (open, locked) = (
                segment(("settings-open", index), "Open", !row.protected, cx.theme()).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.set_protected(index, false, window, cx)
                    }),
                ),
                segment(
                    ("settings-protected", index),
                    "Protected",
                    row.protected,
                    cx.theme(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_protected(index, true, window, cx)
                })),
            );
            let row = self.rows.get(index);
            let Some(row) = row else { continue };
            let (foreground, selected_colour) = (cx.theme().foreground, row.colour.clone());
            let mut swatches = Vec::with_capacity(COLOURS.len() + 1);
            for (swatch, (hex, label)) in COLOURS.iter().enumerate() {
                let (hex, label) = (*hex, *label);
                let chosen = selected_colour.as_deref() == Some(hex);
                let fill: Hsla = u32::from_str_radix(&hex[1..], 16)
                    .map(|value| rgb(value).into())
                    .unwrap_or(foreground);
                swatches.push(
                    div()
                        .id(("settings-colour", index * 16 + swatch))
                        .size(px(18.))
                        .rounded_full()
                        .bg(fill)
                        .cursor_pointer()
                        .border_2()
                        .border_color(if chosen { foreground } else { fill })
                        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_colour(index, Some(hex), window, cx);
                        })),
                );
            }
            swatches.push(
                chip(
                    ("settings-colour-none", index),
                    "None",
                    true,
                    selected_colour.is_none(),
                    cx.theme(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_colour(index, None, window, cx);
                })),
            );
            let labelled = |title: &'static str, input: &Entity<InputState>| {
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w(px(110.)).text_sm().text_color(muted).child(title))
                    .child(div().w(px(220.)).child(Input::new(input)))
            };
            clusters = clusters.child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .when(index > 0, |column| column.border_t_1().border_color(border))
                    .when(self.highlighted == Some(index), |column| {
                        column.bg(primary.opacity(0.08))
                    })
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_semibold()
                                    .child(row.name.clone()),
                            )
                            .child(segmented_bar(cx.theme()).child(open).child(locked)),
                    )
                    .child(labelled("Name", &row.display.input))
                    .child(labelled("Group", &row.workspace.input))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .w(px(110.))
                                    .text_sm()
                                    .text_color(muted)
                                    .child("Colour"),
                            )
                            .child(h_flex().gap_2().items_center().children(swatches)),
                    )
                    .child(labelled("Default namespace", &row.namespace.input))
                    .when_some(row.error.clone(), |column, error| {
                        column.child(div().text_xs().text_color(danger).child(error))
                    }),
            );
        }
        if count == 0 {
            clusters = clusters.child(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(muted)
                    .child("No clusters found in your kubeconfig."),
            );
        }

        div()
            .id("settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(680.))
                    .mx_auto()
                    .p_6()
                    .gap_6()
                    .child(div().text_xl().font_semibold().child("Settings"))
                    .child(
                        v_flex().gap_2().child(heading("UPDATES")).child(
                            card()
                                .bg(secondary.opacity(0.4))
                                .child(line(
                                    "Check on startup",
                                    "Asks GitHub for the latest release and sends nothing about \
                                         this machine. It never installs by itself.",
                                    startup.into_any_element(),
                                ))
                                .child(
                                    v_flex()
                                        .px_4()
                                        .py_3()
                                        .gap_1()
                                        .border_t_1()
                                        .border_color(border)
                                        .child(
                                            h_flex()
                                                .gap_4()
                                                .items_center()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_sm()
                                                        .text_color(status_color)
                                                        .child(status),
                                                )
                                                .child(update_actions),
                                        )
                                        .when_some(signed_note, |column, note| {
                                            column.child(
                                                div().text_xs().text_color(muted).child(note),
                                            )
                                        }),
                                ),
                        ),
                    )
                    .child(v_flex().gap_2().child(heading("LOGS")).child(
                        card().bg(secondary.opacity(0.4)).child(line(
                            "Lines to load",
                            "How much history a log view asks for when it opens.",
                            tail.into_any_element(),
                        )),
                    ))
                    .child(
                        v_flex()
                            .gap_2()
                            .child(heading("CLUSTERS"))
                            .child(div().text_xs().text_color(muted).child(
                                "A protected cluster refuses every change from this app: \
                                         scaling, deleting, applying YAML, Helm, silences. Reading \
                                         still works. The default namespace applies the next time \
                                         the cluster connects.",
                            ))
                            .child(clusters),
                    )
                    .when_some(self.path.clone(), |column, path| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("Saved in {path}")),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{label_edit, namespace_edit};

    #[test]
    fn a_blank_namespace_clears_and_a_valid_one_is_trimmed() {
        assert_eq!(namespace_edit(""), Ok(None));
        assert_eq!(namespace_edit("   "), Ok(None));
        assert_eq!(namespace_edit(" shop "), Ok(Some("shop".to_string())));
    }

    #[test]
    fn a_namespace_kubernetes_would_refuse_is_rejected_with_a_reason() {
        assert!(namespace_edit("Shop").is_err());
        assert!(namespace_edit("a_b").is_err());
        assert!(namespace_edit("-a").is_err());
    }

    #[test]
    fn a_name_is_trimmed_blank_clears_it_and_a_long_one_is_refused() {
        assert_eq!(label_edit("  Prod EU "), Ok(Some("Prod EU".to_string())));
        assert_eq!(label_edit("   "), Ok(None));
        assert!(label_edit(&"x".repeat(41)).is_err());
        assert!(label_edit(&"x".repeat(40)).is_ok());
    }
}
