//! A form for creating a kind: fields, not YAML.
//!
//! The form holds no manifest of its own. Every change is turned into YAML by
//! the layout's builder and handed to the Create page's editor, so what is
//! previewed and applied is always the text on screen, and a person can leave
//! the form for the YAML at any point.

use std::{collections::HashMap, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    forms::{self, Field, FieldValue, FormSpec, Kind, SPECS, Values},
    manifest_ops,
};
use k8s_ops::lookup::LookupOption;
use tokio::runtime::Handle;

use crate::ui::{chip, chip_icon, segment, segmented_bar};

/// Suggestions shown under a field: enough to pick from, not a wall.
const MAX_SUGGESTIONS: usize = 8;

pub enum FormEvent {
    /// The manifest the form now describes.
    Changed(String),
}

/// A field's live state.
enum State {
    Text(Entity<InputState>),
    Select(&'static str),
    Pairs(Vec<(Entity<InputState>, Entity<InputState>)>),
}

struct Slot {
    field: &'static Field,
    state: State,
}

pub struct FormView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    spec: &'static FormSpec,
    slots: Vec<Slot>,
    /// Suggestions read from the cluster, by field key.
    options: HashMap<&'static str, Vec<LookupOption>>,
    /// What is wrong, by field key. Said only once something was edited, so a
    /// fresh form is not a wall of red.
    problems: Vec<(&'static str, String)>,
    touched: bool,
    /// The object being edited, whose fields the form changes and whose other
    /// fields it leaves exactly as they are. `None` when creating.
    editing: Option<forms::Object>,
    _subscriptions: Vec<Subscription>,
}

/// Where a form works: which cluster it reads suggestions from, and the
/// namespace it starts in.
pub struct FormContext {
    pub core: Arc<AppCore>,
    pub runtime: Handle,
    pub cluster: SharedString,
    pub namespace: Option<String>,
}

impl EventEmitter<FormEvent> for FormView {}

impl FormView {
    /// A form for a kind that has a layout, made from any view's context.
    pub fn create<T: 'static>(
        place: &FormContext,
        kind: &str,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Option<Entity<Self>> {
        let spec = SPECS.iter().find(|spec| spec.kind == kind)?;
        Some(cx.new(|cx| Self::build(spec, place, None, window, cx)))
    }

    /// A form over an object that exists, for changing the fields its layout
    /// covers. Its name and namespace cannot change, so they are not shown.
    pub fn edit<T: 'static>(
        place: &FormContext,
        kind: &str,
        object: forms::Object,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Option<Entity<Self>> {
        let spec = SPECS.iter().find(|spec| spec.kind == kind)?;
        Some(cx.new(|cx| Self::build(spec, place, Some(object), window, cx)))
    }

    fn build(
        spec: &'static FormSpec,
        place: &FormContext,
        object: Option<forms::Object>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (core, runtime, cluster, namespace) = (
            &place.core,
            &place.runtime,
            place.cluster.clone(),
            place.namespace.as_deref(),
        );
        // What an existing object already holds, to start from.
        let initial: Values = object
            .as_ref()
            .map(|object| forms::read_values(spec, object))
            .unwrap_or_default();
        let text_start = |key: &str| match initial.get(key) {
            Some(FieldValue::Text(text)) => Some(text.clone()),
            _ => None,
        };
        let mut subscriptions = Vec::new();
        let mut slots = Vec::new();
        for field in spec.fields() {
            let state = match field.kind {
                Kind::Select(options) => State::Select(
                    options
                        .iter()
                        .copied()
                        .find(|option| Some(*option) == text_start(field.key).as_deref())
                        .or_else(|| {
                            options
                                .iter()
                                .copied()
                                .find(|option| *option == field.default)
                        })
                        .or_else(|| options.first().copied())
                        .unwrap_or(""),
                ),
                Kind::Pairs { .. } | Kind::Secret => {
                    let existing = match initial.get(field.key) {
                        Some(FieldValue::Pairs(pairs)) => pairs.clone(),
                        _ => Vec::new(),
                    };
                    let mut rows: Vec<_> = existing
                        .iter()
                        .map(|(key, value)| {
                            Self::pair_with(window, cx, &mut subscriptions, key, value)
                        })
                        .collect();
                    // A blank row to type into, as when creating.
                    rows.push(Self::pair(window, cx, &mut subscriptions));
                    State::Pairs(rows)
                }
                _ => {
                    let start = if object.is_some() {
                        text_start(field.key).unwrap_or_default()
                    } else if field.key == "namespace" {
                        namespace.unwrap_or(field.default).to_string()
                    } else {
                        field.default.to_string()
                    };
                    let input = cx.new(|cx| {
                        let mut state = InputState::new(window, cx).placeholder(field.placeholder);
                        if !start.is_empty() {
                            state.set_value(start.clone(), window, cx);
                        }
                        state
                    });
                    let key = field.key;
                    subscriptions.push(cx.subscribe_in(
                        &input,
                        window,
                        move |this, _, event: &InputEvent, _, cx| {
                            this.touched |= matches!(event, InputEvent::Change);
                            if matches!(event, InputEvent::Change) {
                                this.changed(cx);
                            }
                            // Suggestions depend on the namespace and on a field
                            // like the chosen service; reading them on every key
                            // would be a request per letter.
                            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
                                && (key == "namespace" || this.is_depended_on(key))
                            {
                                this.load_options(cx);
                            }
                        },
                    ));
                    State::Text(input)
                }
            };
            slots.push(Slot { field, state });
        }
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            spec,
            slots,
            options: HashMap::new(),
            problems: Vec::new(),
            touched: false,
            editing: object,
            _subscriptions: subscriptions,
        };
        if view.editing.is_some() {
            // Opening an object to edit it is not a change: the editor keeps its
            // text until a field is touched.
            view.problems = view.problems_now(cx);
        } else {
            view.changed(cx);
        }
        view.load_options(cx);
        view
    }

    /// A row that already holds a key and a value.
    fn pair_with(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
        key: &str,
        value: &str,
    ) -> (Entity<InputState>, Entity<InputState>) {
        let row = Self::pair(window, cx, subscriptions);
        row.0
            .update(cx, |input, cx| input.set_value(key.to_string(), window, cx));
        row.1.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
        row
    }

    /// A key and a value box, whose edits count as changes of the form.
    fn pair(
        window: &mut Window,
        cx: &mut Context<Self>,
        subscriptions: &mut Vec<Subscription>,
    ) -> (Entity<InputState>, Entity<InputState>) {
        let key = cx.new(|cx| InputState::new(window, cx).placeholder("key"));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        for input in [&key, &value] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.touched = true;
                        this.changed(cx);
                    }
                },
            ));
        }
        (key, value)
    }

    fn is_depended_on(&self, key: &str) -> bool {
        self.spec.fields().any(
            |field| matches!(field.kind, Kind::Lookup { depends_on: Some(on), .. } if on == key),
        )
    }

    pub fn kind(&self) -> &'static str {
        self.spec.kind
    }

    fn values(&self, cx: &App) -> Values {
        self.slots
            .iter()
            .map(|slot| {
                let value = match &slot.state {
                    State::Text(input) => FieldValue::Text(input.read(cx).value().to_string()),
                    State::Select(choice) => FieldValue::Text((*choice).to_string()),
                    State::Pairs(rows) => FieldValue::Pairs(
                        rows.iter()
                            .map(|(key, value)| {
                                (
                                    key.read(cx).value().to_string(),
                                    value.read(cx).value().to_string(),
                                )
                            })
                            .collect(),
                    ),
                };
                (slot.field.key, value)
            })
            .collect()
    }

    fn text_of(&self, key: &str, cx: &App) -> Option<String> {
        self.slots
            .iter()
            .find_map(|slot| match (&slot.state, slot.field.key == key) {
                (State::Text(input), true) => Some(input.read(cx).value().trim().to_string()),
                _ => None,
            })
    }

    /// Rebuild the manifest and tell the page.
    pub fn changed(&mut self, cx: &mut Context<Self>) {
        let values = self.values(cx);
        self.problems = self.problems_for(&values);
        let manifest = match &self.editing {
            Some(object) => {
                let mut edited = object.clone();
                forms::apply_values(self.spec, &values, &mut edited);
                edited
            }
            None => forms::build(self.spec, &values),
        };
        if let Ok(yaml) = forms::to_yaml(&manifest) {
            cx.emit(FormEvent::Changed(yaml));
        }
        cx.notify();
    }

    fn problems_now(&self, cx: &App) -> Vec<(&'static str, String)> {
        self.problems_for(&self.values(cx))
    }

    /// What is wrong, leaving out the name and namespace of an object being
    /// edited: they are not on the form, and cannot be changed.
    fn problems_for(&self, values: &Values) -> Vec<(&'static str, String)> {
        let mut found = forms::problems(self.spec, values);
        if self.editing.is_some() {
            found.retain(|(key, _)| !matches!(*key, "name" | "namespace"));
        }
        found
    }

    /// Read the suggestions for every field that has some from the cluster.
    fn load_options(&mut self, cx: &mut Context<Self>) {
        let namespace = self.text_of("namespace", cx).filter(|ns| !ns.is_empty());
        for slot in &self.slots {
            let Kind::Lookup {
                source, depends_on, ..
            } = slot.field.kind
            else {
                continue;
            };
            let param = depends_on.and_then(|key| self.text_of(key, cx));
            if depends_on.is_some() && param.as_deref().is_none_or(str::is_empty) {
                continue;
            }
            let (core, cluster, namespace) = (
                Arc::clone(&self.core),
                self.cluster.clone(),
                namespace.clone(),
            );
            let key = slot.field.key;
            let task = self.runtime.spawn(async move {
                manifest_ops::lookup_options(
                    &core,
                    &cluster,
                    source,
                    namespace.as_deref(),
                    param.as_deref(),
                )
                .await
            });
            cx.spawn(async move |this, cx| {
                let found = task.await.unwrap_or_default();
                this.update(cx, |this, cx| {
                    this.options.insert(key, found);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn choose(&mut self, index: usize, choice: &'static str, cx: &mut Context<Self>) {
        if let Some(Slot {
            state: State::Select(current),
            ..
        }) = self.slots.get_mut(index)
        {
            *current = choice;
        }
        self.touched = true;
        self.changed(cx);
    }

    fn add_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut subscriptions = Vec::new();
        let row = Self::pair(window, cx, &mut subscriptions);
        self._subscriptions.append(&mut subscriptions);
        if let Some(Slot {
            state: State::Pairs(rows),
            ..
        }) = self.slots.get_mut(index)
        {
            rows.push(row);
        }
        cx.notify();
    }

    fn remove_row(&mut self, index: usize, row: usize, cx: &mut Context<Self>) {
        if let Some(Slot {
            state: State::Pairs(rows),
            ..
        }) = self.slots.get_mut(index)
            && rows.len() > 1
            && row < rows.len()
        {
            rows.remove(row);
        }
        self.changed(cx);
    }

    fn suggest(
        &mut self,
        index: usize,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Slot {
            state: State::Text(input),
            ..
        }) = self.slots.get(index)
        {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        // Setting a value from code is not announced as a typed change, so the
        // manifest is rebuilt here.
        self.touched = true;
        self.changed(cx);
        // Choosing a service is what makes its ports suggestible.
        self.load_options(cx);
    }

    fn render_field(&self, index: usize, slot: &Slot, cx: &mut Context<Self>) -> AnyElement {
        let (muted, danger) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger)
        };
        let field = slot.field;
        let problem = self
            .touched
            .then(|| self.problems.iter().find(|(key, _)| *key == field.key))
            .flatten()
            .map(|(_, reason)| reason.clone());

        let control: AnyElement = match &slot.state {
            State::Text(input) => {
                let suggestions = self
                    .options
                    .get(field.key)
                    .map(|options| {
                        options
                            .iter()
                            .take(MAX_SUGGESTIONS)
                            .enumerate()
                            .map(|(at, option)| {
                                let value = option.value.clone();
                                chip(
                                    ("form-suggest", index * 100 + at),
                                    if option.label.is_empty() {
                                        option.value.clone()
                                    } else {
                                        option.label.clone()
                                    },
                                    true,
                                    false,
                                    cx.theme(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.suggest(index, value.clone(), window, cx);
                                    },
                                ))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                v_flex()
                    .gap_1()
                    .child(Input::new(input))
                    .when(!suggestions.is_empty(), |column| {
                        column.child(h_flex().gap_1p5().flex_wrap().children(suggestions))
                    })
                    .into_any_element()
            }
            State::Select(current) => {
                let Kind::Select(options) = field.kind else {
                    return div().into_any_element();
                };
                h_flex()
                    .child(options.iter().enumerate().fold(
                        segmented_bar(cx.theme()),
                        |bar, (at, option)| {
                            let option = *option;
                            bar.child(
                                segment(
                                    ("form-select", index * 100 + at),
                                    option,
                                    *current == option,
                                    cx.theme(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.choose(index, option, cx);
                                    },
                                )),
                            )
                        },
                    ))
                    .into_any_element()
            }
            State::Pairs(rows) => {
                let mut column = v_flex().gap_1p5();
                for (at, (key, value)) in rows.iter().enumerate() {
                    column = column.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(130.)).child(Input::new(key)))
                            .child(div().flex_1().min_w_0().child(Input::new(value)))
                            .child(
                                div()
                                    .id(("form-remove", index * 100 + at))
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .child(gpui_kit::component::Icon::new(IconName::Close).size_3())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_row(index, at, cx);
                                    })),
                            ),
                    );
                }
                column
                    .child(
                        h_flex().child(
                            chip_icon(
                                ("form-add", index),
                                IconName::Plus,
                                "Add",
                                true,
                                false,
                                cx.theme(),
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.add_row(index, window, cx);
                                },
                            )),
                        ),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .child(div().text_sm().child(field.label))
                    .when(field.required, |row| {
                        row.child(div().text_sm().text_color(danger).child("*"))
                    }),
            )
            .child(control)
            .when(!field.help.is_empty(), |column| {
                column.child(div().text_xs().text_color(muted).child(field.help))
            })
            .when_some(problem, |column, reason| {
                column.child(div().text_xs().text_color(danger).child(reason))
            })
            .into_any_element()
    }
}

impl Render for FormView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut column = v_flex().p_3().gap_4();
        let mut index = 0;
        for section in self.spec.sections {
            let mut card = v_flex().gap_3().child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(section.title.to_uppercase()),
            );
            let mut shown = 0;
            for _ in section.fields {
                if let Some(slot) = self.slots.get(index) {
                    // An object that exists keeps its name and namespace.
                    let fixed =
                        self.editing.is_some() && matches!(slot.field.key, "name" | "namespace");
                    if !fixed {
                        card = card.child(self.render_field(index, slot, cx));
                        shown += 1;
                    }
                }
                index += 1;
            }
            if shown > 0 {
                column = column.child(card);
            }
        }
        div()
            .id("form-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(column)
    }
}
