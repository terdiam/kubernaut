//! The Labels and taints tab of a node: edit both as lists, see what saving
//! would change, and save once.
//!
//! Nothing is sent until Save, which asks first, naming each change. The patch
//! carries the version the lists were read at, so a node that changed meanwhile
//! is refused rather than overwritten.

use std::{collections::BTreeMap, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, WindowExt,
        button::ButtonVariant,
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, object_ops};
use k8s_ops::{
    node_edit::{self, EFFECTS, NodeTags, Taint},
    node_pods::{self, PodLimit},
};
use tokio::runtime::Handle;

use crate::ui::{
    Emphasis, button, message, pill, segment, segmented_bar, toast_error, toast_success,
};

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

pub struct NodeEditView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    node: SharedString,
    load: Load,
    /// What the node has, as last read.
    before: NodeTags,
    labels: BTreeMap<String, String>,
    taints: Vec<Taint>,
    label_key: Entity<InputState>,
    label_value: Entity<InputState>,
    taint_key: Entity<InputState>,
    taint_value: Entity<InputState>,
    /// What the node allows and runs, as last read.
    pod_limit: PodLimit,
    pod_limit_input: Entity<InputState>,
    /// The kubelet is being reconfigured; this takes a while.
    applying_limit: bool,
    /// Index into [`EFFECTS`] for the taint being added.
    effect: usize,
    /// Why the last Add or Save did not go through.
    problem: Option<SharedString>,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

impl NodeEditView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        node: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let label_key = input("key", window, cx);
        let label_value = input("value", window, cx);
        let taint_key = input("key", window, cx);
        let taint_value = input("value (optional)", window, cx);
        let pod_limit_input = input("max pods", window, cx);

        let on_enter = |target: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            move |this: &mut Self,
                  _: &Entity<InputState>,
                  event: &InputEvent,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    target(this, window, cx);
                }
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&label_key, window, on_enter(Self::add_label)),
            cx.subscribe_in(&label_value, window, on_enter(Self::add_label)),
            cx.subscribe_in(&taint_key, window, on_enter(Self::add_taint)),
            cx.subscribe_in(&taint_value, window, on_enter(Self::add_taint)),
            cx.subscribe_in(&pod_limit_input, window, on_enter(Self::confirm_limit)),
        ];

        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            node,
            load: Load::Loading,
            before: NodeTags::default(),
            labels: BTreeMap::new(),
            taints: Vec::new(),
            label_key,
            label_value,
            taint_key,
            taint_value,
            pod_limit: PodLimit::default(),
            pod_limit_input,
            applying_limit: false,
            effect: 0,
            problem: None,
            saving: false,
            _subscriptions: subscriptions,
        };
        view.reload(cx);
        view.reload_limit(cx);
        view
    }

    /// Read the node again, dropping the draft.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let (core, cluster, node) = (
            Arc::clone(&self.core),
            self.cluster.to_string(),
            self.node.to_string(),
        );
        let task = self
            .runtime
            .spawn(async move { object_ops::read_node_tags(&core, &cluster, &node).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                match result {
                    Ok(tags) => {
                        this.labels = tags.labels.clone();
                        this.taints = tags.taints.clone();
                        this.before = tags;
                        this.load = Load::Ready;
                    }
                    Err(reason) => this.load = Load::Failed(reason.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the node's pod limit and count. Kept apart from the tags: it is
    /// not part of the draft, and listing pods is the slower call.
    fn reload_limit(&mut self, cx: &mut Context<Self>) {
        let (core, cluster, node) = (
            Arc::clone(&self.core),
            self.cluster.to_string(),
            self.node.to_string(),
        );
        let task = self
            .runtime
            .spawn(async move { object_ops::read_pod_limit(&core, &cluster, &node).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(limit)) = task.await {
                this.update(cx, |this, cx| {
                    this.pod_limit = limit;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Ask before restarting the node's kubelet, then do it.
    fn confirm_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying_limit {
            return;
        }
        let text = self.pod_limit_input.read(cx).value().trim().to_string();
        let limit = match text
            .parse::<u32>()
            .map_err(|_| "enter a whole number".to_string())
            .and_then(|n| {
                node_pods::check_limit(n)
                    .map(|()| n)
                    .map_err(|e| e.to_string())
            }) {
            Ok(limit) => limit,
            Err(reason) => {
                self.problem = Some(reason.into());
                cx.notify();
                return;
            }
        };
        if limit == self.pod_limit.capacity {
            self.problem = Some(format!("the limit is already {limit}").into());
            cx.notify();
            return;
        }
        self.problem = None;
        let (node, current) = (self.node.clone(), self.pod_limit);
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let owner = owner.clone();
            let warning = cx.theme().warning;
            dialog
                .title(format!("Set the pod limit of {node} to {limit}?"))
                .description(
                    v_flex()
                        .gap_1()
                        .child(format!(
                            "Now {} ({} pods scheduled).",
                            current.capacity, current.used
                        ))
                        .child(
                            "This runs a privileged pod on the node, edits the kubelet's \
                             maxPods and restarts the kubelet. Running pods are not stopped, \
                             but the node is NotReady for a few seconds.",
                        )
                        .child(div().text_color(warning).child(
                            "The node's pod CIDR must hold that many addresses, or pods will \
                             fail to get an IP.",
                        ))
                        .when(current.used >= current.capacity, |column| {
                            column.child(div().text_color(warning).child(
                                "The node is full. The pod that makes the change needs one free \
                                 slot: delete or evict a pod from the node first.",
                            ))
                        }),
                )
                .ok_text("Apply")
                .show_cancel(true)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    owner
                        .update(cx, |this, cx| this.apply_limit(limit, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn apply_limit(&mut self, limit: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.applying_limit = true;
        cx.notify();
        let (core, cluster, node) = (
            Arc::clone(&self.core),
            self.cluster.to_string(),
            self.node.to_string(),
        );
        let task = self
            .runtime
            .spawn(async move { object_ops::set_pod_limit(&core, &cluster, &node, limit).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.applying_limit = false;
                match result {
                    Ok(()) => {
                        toast_success(window, cx, format!("{} now allows {limit} pods", this.node));
                        this.problem = None;
                    }
                    Err(reason) => toast_error(window, cx, "Could not set the pod limit", reason),
                }
                this.reload_limit(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn changes(&self) -> Vec<String> {
        node_edit::describe(&self.before, &self.labels, &self.taints)
    }

    fn add_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.label_key.read(cx).value().trim().to_string();
        let value = self.label_value.read(cx).value().trim().to_string();
        if let Err(reason) =
            node_edit::check_key(&key).and_then(|()| node_edit::check_value(&value))
        {
            self.problem = Some(reason.into());
            cx.notify();
            return;
        }
        self.labels.insert(key, value);
        self.problem = None;
        for input in [&self.label_key, &self.label_value] {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn add_taint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taint = Taint {
            key: self.taint_key.read(cx).value().trim().to_string(),
            value: self.taint_value.read(cx).value().trim().to_string(),
            effect: EFFECTS
                .get(self.effect)
                .copied()
                .unwrap_or("NoSchedule")
                .to_string(),
        };
        let mut draft = self.taints.clone();
        draft.push(taint);
        if let Err(reason) = node_edit::check_draft(&self.labels, &draft) {
            self.problem = Some(reason.into());
            cx.notify();
            return;
        }
        self.taints = draft;
        self.problem = None;
        for input in [&self.taint_key, &self.taint_value] {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn revert(&mut self, cx: &mut Context<Self>) {
        self.labels = self.before.labels.clone();
        self.taints = self.before.taints.clone();
        self.problem = None;
        cx.notify();
    }

    /// Say what saving does, then do it on a yes.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.changes();
        if changes.is_empty() || self.saving {
            return;
        }
        let evicts = node_edit::evicts(&self.before, &self.taints);
        let node = self.node.clone();
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let owner = owner.clone();
            let warning = cx.theme().warning;
            dialog
                .title(format!("Save changes to {node}?"))
                .description(
                    v_flex()
                        .gap_1()
                        .children(changes.iter().map(|line| div().child(line.clone())))
                        .when(evicts, |column| {
                            column.child(div().text_color(warning).child(
                                "A NoExecute taint evicts the pods running here that do not \
                                 tolerate it.",
                            ))
                        }),
                )
                .ok_text("Save")
                .show_cancel(true)
                .ok_variant(if evicts {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .on_ok(move |_, window, cx| {
                    owner.update(cx, |this, cx| this.save(window, cx)).ok();
                    true
                })
        });
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.saving = true;
        cx.notify();
        let (core, cluster, node) = (
            Arc::clone(&self.core),
            self.cluster.to_string(),
            self.node.to_string(),
        );
        let (before, labels, taints) = (
            self.before.clone(),
            self.labels.clone(),
            self.taints.clone(),
        );
        let task = self.runtime.spawn(async move {
            object_ops::save_node_tags(&core, &cluster, &node, &before, &labels, &taints).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        toast_success(window, cx, format!("Saved {}", this.node));
                        this.problem = None;
                        this.reload(cx);
                    }
                    Err(reason) => {
                        toast_error(window, cx, "Could not save the node", reason);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_pod_limit(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let busy = self.applying_limit;
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(div().text_sm().font_semibold().child("Pod limit"))
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} allowed · {} scheduled",
                        self.pod_limit.capacity, self.pod_limit.used
                    ))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w_40().child(Input::new(&self.pod_limit_input)))
                    .child(
                        button(
                            "apply-pod-limit",
                            if busy { "Applying…" } else { "Apply…" },
                            !busy,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(!busy, |b| {
                            b.on_click(
                                cx.listener(|this, _, window, cx| this.confirm_limit(window, cx)),
                            )
                        }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Sets the kubelet's maxPods on the node, and restarts the kubelet."),
            )
    }

    fn render_labels(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mono = theme.mono_font_family.clone();
        let (muted, border, success, warning) = (
            theme.muted_foreground,
            theme.border,
            theme.success,
            theme.warning,
        );
        let rows = self.labels.iter().map(|(key, value)| {
            let mark = match self.before.labels.get(key) {
                None => Some(success),
                Some(old) if old != value => Some(warning),
                Some(_) => None,
            };
            let removed = key.clone();
            h_flex()
                .id(SharedString::from(format!("label-{key}")))
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded_md()
                .when_some(mark, |row, color| row.bg(color.opacity(0.12)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_family(mono.clone())
                        .child(format!("{key} = {value}")),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("label-rm-{key}")))
                        .cursor_pointer()
                        .text_color(muted)
                        .child(Icon::new(IconName::Close).size_3p5())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.labels.remove(&removed);
                            cx.notify();
                        })),
                )
        });
        v_flex()
            .gap_1()
            .child(section_title("Labels", self.labels.len(), muted))
            .child(
                v_flex()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .p_1()
                    .children(rows),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.label_key)))
                    .child(div().flex_1().child(Input::new(&self.label_value)))
                    .child(
                        button("add-label", "Add", true, Emphasis::Plain, cx.theme()).on_click(
                            cx.listener(|this, _, window, cx| this.add_label(window, cx)),
                        ),
                    ),
            )
    }

    fn render_taints(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mono = theme.mono_font_family.clone();
        let (muted, border, success, warning) = (
            theme.muted_foreground,
            theme.border,
            theme.success,
            theme.warning,
        );
        let rows = self.taints.iter().enumerate().map(|(index, taint)| {
            let added = !self.before.taints.contains(taint);
            h_flex()
                .id(("taint", index))
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded_md()
                .when(added, |row| row.bg(success.opacity(0.12)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_family(mono.clone())
                        .child(taint.render()),
                )
                .when(taint.effect == "NoExecute", |row| {
                    row.child(pill("evicts", warning))
                })
                .child(
                    div()
                        .id(("taint-rm", index))
                        .cursor_pointer()
                        .text_color(muted)
                        .child(Icon::new(IconName::Close).size_3p5())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if index < this.taints.len() {
                                this.taints.remove(index);
                            }
                            cx.notify();
                        })),
                )
        });
        let effects =
            EFFECTS
                .iter()
                .enumerate()
                .fold(segmented_bar(cx.theme()), |bar, (index, effect)| {
                    bar.child(
                        segment(("effect", index), *effect, self.effect == index, cx.theme())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.effect = index;
                                cx.notify();
                            })),
                    )
                });
        v_flex()
            .gap_1()
            .child(section_title("Taints", self.taints.len(), muted))
            .child(
                v_flex()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .p_1()
                    .when(self.taints.is_empty(), |column| {
                        column.child(
                            div()
                                .px_2()
                                .py_0p5()
                                .text_xs()
                                .text_color(muted)
                                .child("No taints: any pod may be scheduled here."),
                        )
                    })
                    .children(rows),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.taint_key)))
                    .child(div().flex_1().child(Input::new(&self.taint_value))),
            )
            .child(
                h_flex().gap_2().child(effects).child(
                    button("add-taint", "Add", true, Emphasis::Plain, cx.theme())
                        .on_click(cx.listener(|this, _, window, cx| this.add_taint(window, cx))),
                ),
            )
    }
}

fn section_title(text: &'static str, count: usize, muted: Hsla) -> Div {
    h_flex()
        .gap_2()
        .items_baseline()
        .child(div().text_sm().font_semibold().child(text))
        .child(div().text_xs().text_color(muted).child(count.to_string()))
}

impl Render for NodeEditView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        match &self.load {
            Load::Loading => return message("Loading…", muted).into_any_element(),
            Load::Failed(reason) => return message(reason.clone(), danger).into_any_element(),
            Load::Ready => {}
        }
        let changes = self.changes();
        let changed = !changes.is_empty();

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("node-edit")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_4()
                    .child(self.render_pod_limit(cx))
                    .child(self.render_labels(cx))
                    .child(self.render_taints(cx)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .when_some(self.problem.clone(), |text, problem| {
                                text.text_color(danger).child(problem)
                            })
                            .when(self.problem.is_none(), |text| {
                                text.text_color(muted).child(if changed {
                                    format!("{} unsaved change(s)", changes.len())
                                } else {
                                    "No changes".to_string()
                                })
                            }),
                    )
                    .child(
                        button(
                            "node-revert",
                            "Revert",
                            changed,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(changed, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.revert(cx)))
                        }),
                    )
                    .child(
                        button(
                            "node-save",
                            if self.saving { "Saving…" } else { "Save…" },
                            changed && !self.saving,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(changed && !self.saving, |b| {
                            b.on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx)))
                        }),
                    ),
            )
            .into_any_element()
    }
}
