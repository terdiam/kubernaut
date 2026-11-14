//! Add a cluster: from a kubeconfig file, a paste, or picked out of
//! `~/.kube/config`, and remove the ones this app holds.
//!
//! The system file is read only when "From ~/.kube/config" is chosen, and what
//! is picked is copied into the app's own folder. Nothing is connected until
//! the cluster's tile is clicked.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, WindowExt,
        button::ButtonVariant,
        h_flex,
        input::{Editor, EditorState, Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore, cluster_ops,
    clusters::{ImportPreview, ImportRequest, ManagedKubeconfig},
};
use tokio::runtime::Handle;

use crate::ui::{
    Emphasis, button_icon, chip, page_title, segment_icon, segmented_bar, toast_error,
    toast_success,
};

/// A kubeconfig is a few kilobytes; a larger file is the wrong one.
const MAX_BYTES: u64 = 1024 * 1024;

pub enum AddClusterEvent {
    /// The set of clusters changed, so the rail must be rebuilt.
    Changed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    File,
    Paste,
    System,
}

/// A kubeconfig on its way in: what it holds and what it would clash with.
struct Draft {
    yaml: String,
    preview: Result<ImportPreview, String>,
    /// One field per clashing context, filled with a free name.
    renames: Vec<(String, Entity<InputState>)>,
}

/// A file name for a pasted or picked document, from what is known about it.
fn label_for(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "imported".to_string())
}

pub struct AddClusterView {
    core: Arc<AppCore>,
    runtime: Handle,
    source: Source,
    paste: Entity<EditorState>,
    /// Contexts of the system kubeconfig and whether each is picked.
    system: Option<Vec<(SharedString, bool)>>,
    draft: Option<Draft>,
    label: Entity<InputState>,
    managed: Vec<ManagedKubeconfig>,
    busy: Option<&'static str>,
    notice: Option<SharedString>,
    _paste: Subscription,
}

impl EventEmitter<AddClusterEvent> for AddClusterView {}

impl AddClusterView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let paste = cx.new(|cx| EditorState::new(window, cx).language("yaml"));
        let subscription =
            cx.subscribe_in(&paste, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_paste(window, cx);
                }
            });
        let label = cx.new(|cx| InputState::new(window, cx).placeholder("File name"));
        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            source: Source::File,
            paste,
            system: None,
            draft: None,
            label,
            managed: cluster_ops::managed(),
            busy: None,
            notice: None,
            _paste: subscription,
        }
    }

    fn select_source(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }
        self.source = source;
        self.draft = None;
        self.notice = None;
        match source {
            Source::System if self.system.is_none() => {
                self.system = Some(
                    cluster_ops::system_contexts()
                        .into_iter()
                        .map(|context| (context.name.into(), false))
                        .collect(),
                );
            }
            Source::Paste => self.on_paste(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Look at a document and set up the review: what it adds, and a free name
    /// for each context that would clash.
    fn set_draft(
        &mut self,
        yaml: String,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preview = cluster_ops::preview(&self.core, &yaml);
        let renames = match &preview {
            Ok(preview) => preview
                .conflicts
                .iter()
                .map(|name| {
                    let suggestion = preview.suggested.get(name).cloned().unwrap_or_default();
                    let input = cx.new(|cx| {
                        let mut state = InputState::new(window, cx).placeholder("New name");
                        state.set_value(suggestion, window, cx);
                        state
                    });
                    (name.clone(), input)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        self.label.update(cx, |input, cx| {
            input.set_value(label.to_string(), window, cx)
        });
        self.draft = Some(Draft {
            yaml,
            preview,
            renames,
        });
        self.notice = None;
        cx.notify();
    }

    fn on_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source != Source::Paste {
            return;
        }
        let text = self.paste.read(cx).value().to_string();
        if text.trim().is_empty() {
            self.draft = None;
            cx.notify();
        } else {
            self.set_draft(text, "pasted", window, cx);
        }
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = chosen
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            if let Some(path) = path {
                this.update_in(cx, |this, window, cx| this.read_file(path, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn read_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = Some("Reading…");
        cx.notify();
        let label = label_for(&path);
        let task = self.runtime.spawn(async move {
            let length = tokio::fs::metadata(&path)
                .await
                .map_err(|err| err.to_string())?
                .len();
            if length > MAX_BYTES {
                return Err("That file is larger than 1 MB, too big for a kubeconfig.".to_string());
            }
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|err| err.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(text) => this.set_draft(text, &label, window, cx),
                    Err(reason) => {
                        this.notice = Some(reason.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn toggle_system(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some((_, picked)) = self.system.as_mut().and_then(|list| list.get_mut(index)) {
            *picked = !*picked;
        }
        cx.notify();
    }

    fn use_system(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted: Vec<String> = self
            .system
            .iter()
            .flatten()
            .filter(|(_, picked)| *picked)
            .map(|(name, _)| name.to_string())
            .collect();
        match cluster_ops::extract_system(&wanted) {
            Ok(yaml) => self.set_draft(yaml, "kube-config", window, cx),
            Err(reason) => {
                self.notice = Some(reason.into());
                cx.notify();
            }
        }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.draft else { return };
        if self.busy.is_some() || draft.preview.is_err() {
            return;
        }
        let renames: BTreeMap<String, String> = draft
            .renames
            .iter()
            .map(|(context, input)| (context.clone(), input.read(cx).value().to_string()))
            .collect();
        let label = self.label.read(cx).value().trim().to_string();
        let request = ImportRequest {
            yaml: draft.yaml.clone(),
            label: if label.is_empty() {
                "imported".into()
            } else {
                label
            },
            renames,
        };
        self.busy = Some("Adding…");
        self.notice = None;
        cx.notify();

        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { cluster_ops::add(&core, &request) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(added) => {
                        this.draft = None;
                        this.paste
                            .update(cx, |editor, cx| editor.set_value("", window, cx));
                        this.managed = cluster_ops::managed();
                        toast_success(window, cx, format!("Added {}", added.join(", ")));
                        cx.emit(AddClusterEvent::Changed);
                    }
                    Err(reason) => this.notice = Some(reason.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_remove(
        &mut self,
        file: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (owner, file) = (owner.clone(), file.clone());
            dialog
                .title(format!("Remove {label}?"))
                .description(
                    "Its clusters disappear from the app and their connections close. \
                     Nothing changes in the clusters themselves, and your own kubeconfig \
                     is not touched.",
                )
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let file = file.clone();
                    owner
                        .update(cx, |this, cx| this.remove(file, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn remove(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = Some("Removing…");
        cx.notify();
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { cluster_ops::remove(&core, &file).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        this.managed = cluster_ops::managed();
                        toast_success(window, cx, "Removed");
                        cx.emit(AddClusterEvent::Changed);
                    }
                    Err(reason) => toast_error(window, cx, "Could not remove it", reason),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_source(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let idle = self.busy.is_none();
        match self.source {
            Source::File => v_flex()
                .gap_2()
                .child(
                    h_flex().child(
                        button_icon(
                            "add-choose",
                            IconName::Upload,
                            "Choose kubeconfig file…",
                            idle,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(idle, |b| {
                            b.on_click(
                                cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                            )
                        }),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("The file is copied; the original is left where it is."),
                )
                .into_any_element(),
            Source::Paste => div()
                .h(px(200.))
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .overflow_hidden()
                .child(Editor::new(&self.paste).appearance(false).h(relative(1.)))
                .into_any_element(),
            Source::System => {
                let list = self.system.as_deref().unwrap_or(&[]);
                if list.is_empty() {
                    return div()
                        .text_sm()
                        .text_color(muted)
                        .child("No contexts found in ~/.kube/config.")
                        .into_any_element();
                }
                let picked = list.iter().any(|(_, picked)| *picked);
                let chips = list.iter().enumerate().map(|(index, (name, picked))| {
                    chip(
                        ("add-system", index),
                        name.clone(),
                        true,
                        *picked,
                        cx.theme(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_system(index, cx)))
                });
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Pick the clusters to add."),
                    )
                    .child(h_flex().gap_2().flex_wrap().children(chips))
                    .child(
                        h_flex().child(
                            button_icon(
                                "add-use-system",
                                IconName::Check,
                                "Use selected",
                                picked,
                                Emphasis::Plain,
                                cx.theme(),
                            )
                            .when(picked, |b| {
                                b.on_click(
                                    cx.listener(|this, _, window, cx| this.use_system(window, cx)),
                                )
                            }),
                        ),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_review(&self, draft: &Draft, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, danger) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground, theme.danger)
        };
        let idle = self.busy.is_none();
        let card = v_flex()
            .p_4()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(border);
        match &draft.preview {
            Err(reason) => card
                .child(div().text_sm().text_color(danger).child(reason.clone()))
                .into_any_element(),
            Ok(preview) => {
                let count = preview.contexts.len();
                card.child(div().font_semibold().child(match count {
                    1 => "Adds 1 cluster".to_string(),
                    n => format!("Adds {n} clusters"),
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(preview.contexts.join(", ")),
                )
                .children(draft.renames.iter().map(|(context, input)| {
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(format!(
                            "A cluster named {context} already exists. Save this one as:"
                        )))
                        .child(div().w(px(260.)).child(Input::new(input)))
                }))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(div().text_sm().text_color(muted).child("Saved as"))
                        .child(div().w(px(200.)).child(Input::new(&self.label))),
                )
                .child(
                    h_flex().child(
                        button_icon(
                            "add-confirm",
                            IconName::Plus,
                            if count == 1 {
                                "Add cluster"
                            } else {
                                "Add clusters"
                            },
                            idle,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(idle, |b| {
                            b.on_click(cx.listener(|this, _, window, cx| this.add(window, cx)))
                        }),
                    ),
                )
                .into_any_element()
            }
        }
    }

    fn render_managed(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground)
        };
        let idle = self.busy.is_none();
        let mut card = v_flex()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .overflow_hidden();
        if self.managed.is_empty() {
            card = card.child(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(muted)
                    .child("No clusters added yet."),
            );
        }
        for (index, managed) in self.managed.iter().enumerate() {
            let (file, label) = (managed.file.clone(), managed.label.clone());
            let remove = cx.listener(move |this, _, window, cx| {
                this.confirm_remove(file.clone(), label.clone(), window, cx);
            });
            card = card.child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .items_center()
                    .when(index > 0, |row| row.border_t_1().border_color(border))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_semibold().child(managed.label.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(managed.contexts.join(", ")),
                            ),
                    )
                    .child(
                        chip(("add-remove", index), "Remove", idle, false, cx.theme())
                            .when(idle, |c| c.on_click(remove)),
                    ),
            );
        }
        card
    }
}

impl Render for AddClusterView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, danger) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger)
        };
        let sources = [
            (Source::File, IconName::Upload, "File"),
            (Source::Paste, IconName::Copy, "Paste"),
            (Source::System, IconName::Folder, "From ~/.kube/config"),
        ]
        .into_iter()
        .fold(segmented_bar(cx.theme()), |bar, (source, icon, label)| {
            bar.child(
                segment_icon(
                    ("add-source", source as usize),
                    icon,
                    label,
                    self.source == source,
                    cx.theme(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_source(source, window, cx);
                })),
            )
        });
        let body = self.render_source(cx);
        let review = self
            .draft
            .as_ref()
            .map(|draft| self.render_review(draft, cx).into_any_element());
        let managed = self.render_managed(cx).into_any_element();

        div()
            .id("add-cluster")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(720.))
                    .mx_auto()
                    .p_6()
                    .gap_5()
                    .child(page_title(IconName::Plus, "Add a cluster", cx.theme()).text_xl())
                    .child(div().text_sm().text_color(muted).child(
                        "Clusters are copied into this app's own folder. Your \
                             ~/.kube/config is never changed, and nothing connects until you \
                             click the cluster.",
                    ))
                    .child(h_flex().child(sources))
                    .child(body)
                    .when_some(self.busy, |column, text| {
                        column.child(div().text_xs().text_color(muted).child(text))
                    })
                    .when_some(self.notice.clone(), |column, text| {
                        column.child(div().text_sm().text_color(danger).child(text))
                    })
                    .children(review)
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted)
                            .child("YOUR CLUSTERS"),
                    )
                    .child(managed),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::label_for;
    use std::path::Path;

    #[test]
    fn a_file_name_comes_from_the_file_without_its_extension() {
        assert_eq!(label_for(Path::new("/home/me/prod.yaml")), "prod");
        assert_eq!(label_for(Path::new("config")), "config");
        assert_eq!(label_for(Path::new("/")), "imported");
    }
}
