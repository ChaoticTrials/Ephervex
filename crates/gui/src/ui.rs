slint::slint! {
    import { AboutSlint, ComboBox, ScrollView, Slider, Spinner } from "std-widgets.slint";

    export global GuiI18n {
        pure callback text(string) -> string;
        pure callback count(string, int) -> string;
        pure callback value(string, string) -> string;
        pure callback values(string, string, string) -> string;
    }

    export global Theme {
        in-out property <float> text-scale: 1;
        out property <length> caption-size: 11px * text-scale;
        out property <length> label-size: 12px * text-scale;
        out property <length> body-size: 13px * text-scale;
        out property <length> row-heading-size: 14px * text-scale;
        out property <length> section-size: 16px * text-scale;
        out property <length> heading-size: 17px * text-scale;
        out property <length> item-title-size: 18px * text-scale;
        out property <length> brand-size: 20px * text-scale;
        out property <length> workspace-title-size: 23px * text-scale;
        out property <length> about-title-size: 24px * text-scale;
        out property <length> diagnostics-title-size: 25px * text-scale;
        out property <length> page-title-size: 27px * text-scale;
        out property <color> background: #101315;
        out property <color> surface: #171b1e;
        out property <color> elevated: #1e2428;
        out property <color> border: #30373c;
        out property <color> accent: #5dbbc3;
        out property <color> accent-secondary: #78a8c2;
        out property <color> success: #62c58b;
        out property <color> warning: #ddb76a;
        out property <color> danger: #e06c75;
        out property <color> text: #e8ecef;
        out property <color> text-secondary: #a6b0b6;
        out property <color> text-muted: #6f797f;
        out property <color> server: #a888df;
        out property <color> vanilla: #8fb8cd;
        out property <color> forge: #d8a653;
        out property <color> neoforge: #65c58a;
    }

    export struct InstanceRow {
        id: string,
        name: string,
        minecraft: string,
        loader: string,
        loader-kind: string,
        storage: string,
        storage-kind: string,
        source: string,
        path: string,
        updated: string,
        linked-servers: int,
        mods: int,
        needs-attention: bool,
        state: string,
        state-kind: string,
        can-launch: bool,
        connect-server-id: string,
        connect-server-name: string,
        can-connect: bool,
    }

    export struct ServerRow {
        id: string,
        name: string,
        minecraft: string,
        loader: string,
        loader-kind: string,
        storage: string,
        storage-kind: string,
        relationship: string,
        linked-client-id: string,
        state: string,
        state-kind: string,
        state-detail: string,
        address: string,
        path: string,
        installed: bool,
        needs-attention: bool,
        can-start: bool,
        can-stop: bool,
        can-restart: bool,
        can-kill: bool,
        can-send-command: bool,
    }

    export struct ModRow {
        project-id: string,
        title: string,
        version: string,
        provider: string,
        origin: string,
    }

    export struct ModSearchRow {
        project-id: string,
        title: string,
        description: string,
        author: string,
        downloads: string,
    }

    export struct DiagnosticRow {
        scope-kind: string,
        scope-id: string,
        entity: string,
        kind: string,
        name: string,
        path: string,
        modified: string,
        size: string,
        summary: string,
    }

    export struct ActivityRow {
        id: string,
        title: string,
        entity: string,
        status: string,
        detail: string,
        time: string,
        progress: float,
        indeterminate: bool,
        active: bool,
        failed: bool,
        cancellable: bool,
    }

    export struct AccountRow {
        id: string,
        name: string,
        kind: string,
        is-default: bool,
        credential-state: string,
    }

    export struct JavaRow {
        path: string,
        version: string,
        vendor: string,
        origin: string,
        compatible: string,
    }

    export struct PreferenceRow {
        key: string,
        label: string,
        value: string,
        customized: bool,
        kind: string,
        syntax: string,
        numeric-value: float,
    }

    export struct PendingEntityRow {
        id: string,
        name: string,
        target: string,
        loader-kind: string,
        storage: string,
        storage-kind: string,
        state: string,
        server: bool,
    }

    component BodyText inherits Text {
        color: Theme.text-secondary;
        font-size: Theme.body-size;
        vertical-alignment: center;
        vertical-stretch: 0;
    }

    component Heading inherits Text {
        color: Theme.text;
        font-size: Theme.heading-size;
        font-weight: 600;
        vertical-alignment: center;
        horizontal-stretch: 0;
        vertical-stretch: 0;
    }

    component Eyebrow inherits Text {
        color: Theme.text-muted;
        font-size: Theme.caption-size;
        font-weight: 600;
        letter-spacing: 0.7px;
        vertical-stretch: 0;
    }

    component PageIntro inherits VerticalLayout {
        in property <string> title;
        in property <string> subtitle;
        spacing: 5px;
        horizontal-stretch: 1;
        Heading { text: root.title; font-size: Theme.page-title-size; }
        if root.subtitle != "" : BodyText {
            text: root.subtitle;
            color: Theme.text-secondary;
            wrap: word-wrap;
        }
    }

    component SectionTitle inherits VerticalLayout {
        in property <string> title;
        in property <string> detail;
        spacing: 4px;
        Heading { text: root.title; font-size: Theme.section-size; }
        if root.detail != "" : BodyText {
            text: root.detail;
            color: Theme.text-muted;
            wrap: word-wrap;
        }
    }

    component Divider inherits Rectangle {
        height: 1px;
        vertical-stretch: 0;
        background: Theme.border;
    }

    component AppButton inherits Rectangle {
        in property <string> text;
        in property <string> tone: "secondary";
        in property <bool> enabled: true;
        callback clicked();
        min-width: max(88px, label.preferred-width + 28px);
        height: 36px;
        horizontal-stretch: 0;
        border-radius: 6px;
        border-width: 1px;
        border-color: tone == "primary" ? Theme.accent : tone == "danger" ? Theme.danger : Theme.border;
        background: !enabled ? Theme.surface : area.pressed ? Theme.background : area.has-hover ? (tone == "primary" ? rgb(54, 139, 147) : tone == "danger" ? rgb(123, 48, 56) : Theme.elevated) : tone == "primary" ? rgb(45, 125, 132) : tone == "danger" ? rgb(97, 39, 46) : Theme.surface;
        accessible-role: button;
        accessible-label: text;
        accessible-enabled: enabled;
        accessible-action-default => { if root.enabled { root.clicked(); } }
        label := Text {
            text: root.text;
            color: !root.enabled ? Theme.text-muted : root.tone == "danger" ? #fff0f1 : Theme.text;
            font-size: Theme.body-size;
            font-weight: root.tone == "primary" ? 600 : 400;
            horizontal-alignment: center;
            vertical-alignment: center;
        }
        area := TouchArea {
            enabled: root.enabled;
            clicked => { root.clicked(); }
        }
    }

    component IconButton inherits Rectangle {
        in property <image> icon;
        in property <string> accessible-name;
        in property <bool> enabled: true;
        callback clicked();
        width: 36px;
        height: 36px;
        border-radius: 6px;
        border-width: 1px;
        border-color: area.has-hover ? Theme.accent : Theme.border;
        background: area.has-hover ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: accessible-name;
        accessible-enabled: enabled;
        accessible-action-default => { if root.enabled { root.clicked(); } }
        Image { x: 8px; y: 8px; width: parent.width - 16px; height: parent.height - 16px; source: root.icon; image-fit: contain; }
        area := TouchArea { enabled: root.enabled; clicked => { root.clicked(); } }
    }

    component TextField inherits Rectangle {
        in-out property <string> text <=> input.text;
        in property <string> placeholder;
        in property <bool> enabled: true;
        in property <bool> invalid: false;
        in property <InputType> input-type: InputType.text;
        callback accepted();
        callback edited(string);
        height: 36px;
        min-width: 120px;
        border-radius: 6px;
        border-width: 1px;
        border-color: invalid ? Theme.danger : input.has-focus ? Theme.accent : Theme.border;
        background: Theme.background;
        input := TextInput {
            x: 11px; width: parent.width - 22px; height: parent.height;
            color: root.enabled ? Theme.text : Theme.text-muted;
            font-size: Theme.body-size;
            enabled: root.enabled;
            input-type: root.input-type;
            single-line: true;
            vertical-alignment: center;
            accepted => { root.accepted(); }
            edited => { root.edited(self.text); }
        }
        if root.text == "" : Text {
            x: 11px; width: parent.width - 22px; height: parent.height;
            text: root.placeholder;
            color: Theme.text-muted;
            font-size: Theme.body-size;
            vertical-alignment: center;
        }
    }

    component FieldLabel inherits Text {
        color: Theme.text-secondary;
        font-size: Theme.label-size;
    }

    component SelectableText inherits Rectangle {
        in property <string> text;
        in property <TextWrap> wrap: no-wrap;
        in property <string> font-family: "monospace";
        in property <length> font-size: 12px;
        background: rgb(11, 14, 16);
        forward-focus: selectable-input;
        accessible-role: text-input;
        accessible-read-only: true;
        accessible-value: text;
        ContextMenuArea {
            Menu {
                MenuItem { title: GuiI18n.text("gui-copy"); enabled: root.text != ""; activated => { selectable-input.copy(); } }
                MenuItem { title: GuiI18n.text("gui-select-all"); enabled: root.text != ""; activated => { selectable-input.select-all(); } }
            }
            selectable-scroll := ScrollView {
                viewport-width: root.wrap == TextWrap.no-wrap ? max(self.visible-width, selectable-input.preferred-width + 16px) : self.visible-width;
                viewport-height: max(self.visible-height, selectable-input.preferred-height + 16px);
                selectable-input := TextInput {
                    x: 8px; y: 8px;
                    width: root.wrap == TextWrap.no-wrap ? max(selectable-scroll.visible-width - 16px, self.preferred-width) : selectable-scroll.visible-width - 16px;
                    height: max(selectable-scroll.visible-height - 16px, self.preferred-height);
                    text: root.text;
                    color: Theme.text-secondary;
                    selection-background-color: Theme.accent;
                    selection-foreground-color: Theme.background;
                    read-only: true;
                    single-line: false;
                    wrap: root.wrap;
                    font-family: root.font-family;
                    font-size: root.font-size;
                    page-height: selectable-scroll.visible-height;
                }
            }
        }
    }

    component Toggle inherits Rectangle {
        in-out property <bool> checked;
        in property <bool> enabled: true;
        callback toggled(bool);
        width: 42px;
        height: 23px;
        horizontal-stretch: 0;
        border-radius: 12px;
        border-width: 1px;
        border-color: checked ? Theme.accent : Theme.border;
        background: checked ? #285e63 : Theme.background;
        accessible-role: checkbox;
        accessible-checked: checked;
        accessible-enabled: enabled;
        accessible-action-default => { if root.enabled { root.checked = !root.checked; root.toggled(root.checked); } }
        Rectangle {
            x: root.checked ? 21px : 3px;
            y: 3px;
            width: 17px; height: 17px;
            border-radius: 9px;
            background: root.enabled ? (root.checked ? Theme.accent : Theme.text-secondary) : Theme.text-muted;
            animate x { duration: 100ms; }
        }
        TouchArea {
            enabled: root.enabled;
            clicked => { root.checked = !root.checked; root.toggled(root.checked); }
        }
    }

    component LabeledToggle inherits Rectangle {
        in property <string> label;
        in-out property <bool> checked;
        in property <bool> enabled: true;
        callback toggled(bool);
        width: 210px;
        height: 32px;
        horizontal-stretch: 0;
        HorizontalLayout { spacing: 10px;
            BodyText { text: root.label; color: root.enabled ? Theme.text-secondary : Theme.text-muted; }
            Rectangle { horizontal-stretch: 1; }
            Toggle { checked <=> root.checked; enabled: root.enabled; toggled(value) => { root.toggled(value); } }
        }
    }

    component Pill inherits Rectangle {
        in property <string> text;
        in property <color> tone-color: Theme.text-secondary;
        height: 24px;
        width: label.preferred-width + 18px;
        horizontal-stretch: 0;
        border-radius: 5px;
        border-width: 1px;
        border-color: root.tone-color.with-alpha(0.55);
        background: root.tone-color.with-alpha(0.12);
        label := Text {
            text: root.text;
            color: root.tone-color;
            font-size: Theme.caption-size;
            font-weight: 600;
            horizontal-alignment: center;
            vertical-alignment: center;
        }
    }

    component NavItem inherits Rectangle {
        in property <image> icon;
        in property <string> label;
        in property <bool> selected;
        callback clicked();
        height: 46px;
        border-radius: 6px;
        background: selected ? #192d32 : area.has-hover ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: label;
        accessible-action-default => { root.clicked(); }
        Rectangle { x: 0px; y: 8px; width: 3px; height: 30px; background: root.selected ? Theme.accent : transparent; border-radius: 2px; }
        HorizontalLayout {
            alignment: center;
            padding-left: 16px; padding-right: 12px; spacing: 13px;
            Image { source: root.icon; width: 22px; height: 22px; image-fit: contain; }
            Text { text: root.label; color: root.selected ? Theme.accent : Theme.text-secondary; font-size: Theme.row-heading-size; vertical-alignment: center; }
        }
        area := TouchArea { clicked => { root.clicked(); } }
    }

    component SettingsNavItem inherits Rectangle {
        in property <string> label;
        in property <bool> selected;
        callback clicked();
        height: 42px;
        border-radius: 6px;
        background: selected ? #192d32 : area.has-hover ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: label;
        accessible-action-default => { root.clicked(); }
        Rectangle {
            x: 0px; y: 7px; width: 3px; height: 28px;
            border-radius: 2px;
            background: root.selected ? Theme.accent : transparent;
        }
        Text {
            x: 16px; width: parent.width - 26px; height: parent.height;
            text: root.label;
            color: root.selected ? Theme.accent : Theme.text-secondary;
            font-size: Theme.body-size;
            font-weight: root.selected ? 600 : 400;
            vertical-alignment: center;
        }
        area := TouchArea { clicked => { root.clicked(); } }
    }

    component WorkspaceTab inherits Rectangle {
        in property <string> text;
        in property <bool> selected;
        callback clicked();
        min-width: max(112px, label.preferred-width + 30px);
        height: 38px;
        horizontal-stretch: 0;
        border-radius: 5px;
        background: area.has-hover ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: text;
        accessible-action-default => { root.clicked(); }
        label := Text {
            text: root.text;
            color: root.selected ? Theme.text : Theme.text-secondary;
            font-size: Theme.body-size;
            font-weight: root.selected ? 600 : 400;
            horizontal-alignment: center;
            vertical-alignment: center;
        }
        Rectangle {
            x: 12px; y: parent.height - 2px; width: parent.width - 24px; height: 2px;
            border-radius: 1px;
            background: root.selected ? Theme.accent : transparent;
        }
        area := TouchArea { clicked => { root.clicked(); } }
    }

    component LoaderGlyph inherits Rectangle {
        in property <string> kind;
        in property <bool> server: false;
        width: 52px; height: 52px;
        border-radius: 8px;
        border-width: 1px;
        border-color: server ? Theme.server : kind == "forge" ? Theme.forge : kind == "neoforge" ? Theme.neoforge : Theme.vanilla;
        background: root.border-color.with-alpha(0.09);
        if !root.server : Image {
            x: 7px; y: 7px; width: 38px; height: 38px;
            source: root.kind == "forge" ? @image-url("../../../assets/icons/environment-forge.svg") : root.kind == "neoforge" ? @image-url("../../../assets/icons/environment-neoforge.svg") : @image-url("../../../assets/icons/environment-vanilla.svg");
            image-fit: contain;
        }
        if root.server : Image { x: 11px; y: 11px; width: 30px; height: 30px; source: @image-url("../../../assets/icons/semantic-server.svg"); image-fit: contain; }
    }

    component EmptyState inherits Rectangle {
        in property <string> title;
        in property <string> message;
        callback primary();
        callback secondary();
        in property <string> primary-text;
        in property <string> secondary-text;
        background: transparent;
        VerticalLayout {
            alignment: center; spacing: 10px; padding: 32px;
            Image { source: @image-url("../../../assets/icons/environment-generic.svg"); width: 64px; height: 64px; image-fit: contain; }
            Heading { text: root.title; horizontal-alignment: center; }
            BodyText { text: root.message; horizontal-alignment: center; wrap: word-wrap; }
            Rectangle { height: 5px; }
            HorizontalLayout {
                alignment: center; spacing: 8px;
                if root.secondary-text != "" : AppButton { text: root.secondary-text; clicked => { root.secondary(); } }
                if root.primary-text != "" : AppButton { text: root.primary-text; tone: "primary"; clicked => { root.primary(); } }
            }
        }
    }

    component PreferenceCard inherits Rectangle {
        in property <PreferenceRow> item;
        in property <bool> compact;
        callback save(string, string);
        callback reset(string);
        height: 92px;
        border-radius: 8px;
        border-width: 1px;
        border-color: Theme.border;
        background: Theme.surface;
        HorizontalLayout {
            padding: 14px;
            spacing: 14px;
            VerticalLayout {
                width: root.compact ? 170px : 245px;
                spacing: 4px;
                HorizontalLayout { spacing: 8px;
                    Heading { text: item.label; font-size: Theme.row-heading-size; }
                    if item.customized : Pill { text: GuiI18n.text("gui-customized"); tone-color: Theme.accent-secondary; }
                }
                BodyText { text: item.syntax; color: Theme.text-muted; wrap: word-wrap; }
            }
            Rectangle {
                horizontal-stretch: 1;
                if item.kind == "boolean" : HorizontalLayout {
                    alignment: end;
                    spacing: 10px;
                    BodyText { text: item.value == "true" ? GuiI18n.text("gui-enabled") : GuiI18n.text("gui-disabled"); }
                    Toggle { checked: item.value == "true"; toggled(value) => { root.save(item.key, value ? "true" : "false"); } }
                    if item.customized : AppButton { text: GuiI18n.text("gui-reset"); clicked => { root.reset(item.key); } }
                }
                if item.kind == "choice" : HorizontalLayout {
                    alignment: end;
                    spacing: 8px;
                    ComboBox { model: ["auto", "1", "2", "3", "4"]; current-index: item.value == "1" ? 1 : item.value == "2" ? 2 : item.value == "3" ? 3 : item.value == "4" ? 4 : 0; selected(value) => { root.save(item.key, value); } width: 210px; }
                    if item.customized : AppButton { text: GuiI18n.text("gui-reset"); clicked => { root.reset(item.key); } }
                }
                if item.kind == "slider" : HorizontalLayout {
                    spacing: 12px;
                    fov := Slider { minimum: 30; maximum: 110; step: 1; value: item.numeric-value; released(value) => { root.save(item.key, "" + round(value)); } }
                    BodyText { text: round(fov.value) == 70 ? GuiI18n.text("gui-fov-normal") : round(fov.value) == 110 ? GuiI18n.text("gui-fov-quake-pro") : round(fov.value) + " deg"; width: 90px; }
                    if item.customized : AppButton { text: GuiI18n.text("gui-reset"); clicked => { root.reset(item.key); } }
                }
                if item.kind == "integer" || item.kind == "text" : HorizontalLayout {
                    spacing: 8px;
                    pref-editor := TextField { text: item.value; input-type: item.kind == "integer" ? InputType.number : InputType.text; horizontal-stretch: 1; accepted => { root.save(item.key, self.text); } }
                    AppButton { text: GuiI18n.text("gui-apply"); width: 92px; clicked => { root.save(item.key, pref-editor.text); } }
                    if item.customized : AppButton { text: GuiI18n.text("gui-reset"); clicked => { root.reset(item.key); } }
                }
            }
        }
    }

    component PendingCard inherits Rectangle {
        in property <PendingEntityRow> item;
        height: 112px;
        border-radius: 8px;
        border-width: 1px;
        border-color: Theme.accent.with-alpha(0.7);
        background: Theme.surface;
        HorizontalLayout { padding: 15px; spacing: 16px; alignment: center;
            LoaderGlyph { kind: item.loader-kind; server: item.server; }
            VerticalLayout { horizontal-stretch: 1; spacing: 5px;
                Heading { text: item.name; }
                BodyText { text: item.target; wrap: word-wrap; }
                BodyText { text: GuiI18n.text("gui-creation-pending-detail"); color: Theme.text-muted; wrap: word-wrap; }
            }
            VerticalLayout { width: 140px; alignment: center; spacing: 7px;
                Pill { text: item.storage; tone-color: item.storage-kind == "temporary" ? Theme.warning : Theme.success; }
                HorizontalLayout { alignment: center; spacing: 7px; Spinner { width: 18px; height: 18px; } BodyText { text: item.state; color: Theme.accent; } }
            }
        }
    }

    component ToastPanel inherits Rectangle {
        in property <string> message;
        in property <bool> error;
        callback dismiss();
        width: 420px;
        height: max(72px, toast-message.preferred-height + 38px);
        horizontal-stretch: 0;
        border-radius: 8px;
        border-width: 1px;
        border-color: error ? Theme.danger.with-alpha(0.75) : Theme.success.with-alpha(0.75);
        background: Theme.elevated;
        Rectangle { x: 0px; y: 0px; width: 4px; height: parent.height; background: root.error ? Theme.danger : Theme.success; border-radius: 3px; }
        HorizontalLayout { padding-left: 16px; padding-right: 10px; padding-top: 10px; padding-bottom: 10px; spacing: 10px; alignment: center;
            VerticalLayout { horizontal-stretch: 1; spacing: 3px; Heading { text: root.error ? GuiI18n.text("gui-notification-error") : GuiI18n.text("gui-notification-complete"); font-size: Theme.body-size; } toast-message := BodyText { text: root.message; color: Theme.text-secondary; wrap: word-wrap; } }
            IconButton { icon: @image-url("../../../assets/icons/close.svg"); accessible-name: GuiI18n.text("gui-dismiss-notification"); width: 32px; height: 32px; clicked => { root.dismiss(); } }
        }
    }

    component ActivityCard inherits Rectangle {
        in property <ActivityRow> item;
        callback cancel(string);
        height: max(102px, detail-text.preferred-height + 82px);
        border-radius: 8px;
        border-width: 1px;
        border-color: item.failed ? Theme.danger : item.active ? Theme.accent : Theme.border;
        background: Theme.surface;
        HorizontalLayout { padding: 14px; spacing: 14px;
            VerticalLayout { horizontal-stretch: 1; spacing: 4px;
                Heading { text: item.title; font-size: Theme.row-heading-size; }
                BodyText { text: item.entity; color: Theme.text; wrap: word-wrap; }
                detail-text := BodyText { text: item.detail; visible: item.detail != ""; wrap: word-wrap; }
                BodyText { text: item.id + " | " + item.time; color: Theme.text-muted; font-family: "monospace"; font-size: Theme.caption-size; }
                if item.active && !item.indeterminate : Rectangle { height: 4px; border-radius: 2px; background: Theme.border; Rectangle { width: parent.width * item.progress; height: parent.height; border-radius: 2px; background: Theme.accent; } }
            }
            VerticalLayout { width: 112px; alignment: start; spacing: 8px;
                Pill { text: item.status; tone-color: item.failed ? Theme.danger : item.active ? Theme.accent : Theme.success; }
                if item.cancellable : AppButton { width: 112px; text: GuiI18n.text("gui-cancel"); clicked => { root.cancel(item.id); } }
            }
        }
    }

    component InstanceCard inherits Rectangle {
        in property <InstanceRow> item;
        in property <bool> selected;
        callback choose(string);
        callback launch(string);
        callback open-workspace(string);
        callback open-folder(string);
        callback create-server(string, string);
        callback duplicate(string);
        callback delete(string, string);
        height: 112px;
        border-radius: 8px;
        border-width: selected ? 2px : 1px;
        border-color: selected ? Theme.accent : Theme.border;
        background: selected ? #17272c : card-area.has-hover ? Theme.elevated : Theme.surface;
        accessible-role: button;
        accessible-label: item.name;
        accessible-action-default => { root.choose(item.id); }
        Rectangle {
            x: 0px; y: 10px; width: 3px; height: parent.height - 20px;
            border-radius: 2px;
            background: root.selected ? Theme.accent : transparent;
        }
        HorizontalLayout {
            padding: 15px; spacing: 16px;
            LoaderGlyph { kind: item.loader-kind; }
            VerticalLayout {
                spacing: 5px;
                HorizontalLayout {
                    spacing: 9px;
                    Heading { text: item.name; }
                }
                BodyText { text: GuiI18n.values("gui-target-summary", item.minecraft, item.loader) + (item.mods > 0 ? " | " + GuiI18n.count("gui-managed-mods-count", item.mods) : ""); }
                BodyText { text: item.linked-servers > 0 ? GuiI18n.count("gui-linked-servers-count", item.linked-servers) : GuiI18n.value("gui-source-instance", item.source); color: item.linked-servers > 0 ? Theme.accent : Theme.text-muted; }
            }
            VerticalLayout {
                width: 224px; alignment: center; spacing: 7px;
                HorizontalLayout {
                    alignment: center; spacing: 7px;
                    BodyText { text: item.storage; color: item.storage-kind == "temporary" ? Theme.warning : Theme.success; font-size: Theme.caption-size; }
                    Pill { text: item.state; tone-color: item.state-kind == "running" ? Theme.success : item.state-kind == "starting" ? Theme.warning : Theme.text-secondary; }
                    if item.needs-attention : Pill { text: GuiI18n.text("gui-attention"); tone-color: Theme.danger; }
                }
                HorizontalLayout { alignment: center; spacing: 7px;
                    if item.can-launch : AppButton { width: 150px; text: GuiI18n.text("gui-launch"); tone: "primary"; clicked => { root.launch(item.id); } }
                    if !item.can-launch : Rectangle { width: 150px; }
                    IconButton { icon: @image-url("../../../assets/icons/open-workspace.svg"); accessible-name: GuiI18n.text("gui-open-workspace"); clicked => { root.open-workspace(item.id); } }
                }
            }
        }
        card-area := TouchArea { width: parent.width - 250px; clicked => { root.choose(item.id); } double-clicked => { root.open-workspace(item.id); } }
        ContextMenuArea {
            Menu {
                MenuItem { title: GuiI18n.text("gui-open-workspace"); activated => { root.open-workspace(item.id); } }
                MenuItem { title: GuiI18n.text("gui-launch"); enabled: item.can-launch; activated => { root.launch(item.id); } }
                MenuItem { title: GuiI18n.text("gui-open-folder"); activated => { root.open-folder(item.id); } }
                MenuItem { title: GuiI18n.text("gui-create-linked-server"); activated => { root.create-server(item.id, item.name); } }
                MenuSeparator { }
                MenuItem { title: GuiI18n.text("gui-duplicate-instance"); activated => { root.duplicate(item.id); } }
                MenuItem { title: GuiI18n.text("gui-delete-instance"); activated => { root.delete(item.id, item.name); } }
            }
        }
    }

    component ServerCard inherits Rectangle {
        in property <ServerRow> item;
        in property <bool> selected;
        callback choose(string);
        callback primary(string);
        callback open-workspace(string);
        callback open-folder(string);
        callback restart(string);
        callback kill(string, string);
        callback delete(string, string);
        height: 112px;
        border-radius: 8px;
        border-width: selected ? 2px : 1px;
        border-color: selected ? Theme.accent : Theme.border;
        background: selected ? #17272c : card-area.has-hover ? Theme.elevated : Theme.surface;
        accessible-role: button;
        accessible-label: item.name;
        accessible-action-default => { root.choose(item.id); }
        Rectangle {
            x: 0px; y: 10px; width: 3px; height: parent.height - 20px;
            border-radius: 2px;
            background: root.selected ? Theme.accent : transparent;
        }
        HorizontalLayout {
            padding: 15px; spacing: 16px;
            LoaderGlyph { kind: item.loader-kind; server: true; }
            VerticalLayout {
                spacing: 5px;
                HorizontalLayout {
                    spacing: 9px;
                    Heading { text: item.name; }
                }
                BodyText { text: GuiI18n.values("gui-target-summary", item.minecraft, item.loader) + " | " + item.relationship; }
                BodyText { text: item.address + " | " + item.state-detail; color: Theme.text-muted; }
            }
            VerticalLayout {
                width: 224px; alignment: center; spacing: 7px;
                HorizontalLayout {
                    alignment: center; spacing: 7px;
                    BodyText { text: item.storage; color: item.storage-kind == "temporary" ? Theme.warning : Theme.success; font-size: Theme.caption-size; }
                    Pill { text: item.state; tone-color: item.state-kind == "running" ? Theme.success : item.state-kind == "stopped" ? Theme.text-secondary : Theme.warning; }
                }
                HorizontalLayout { alignment: center; spacing: 7px;
                    if item.can-start || item.can-stop : AppButton {
                        width: 150px;
                        text: item.can-start ? GuiI18n.text("gui-start") : GuiI18n.text("gui-stop");
                        tone: "primary";
                        clicked => { root.primary(item.id); }
                    }
                    if !item.can-start && !item.can-stop : Rectangle { width: 150px; }
                    IconButton { icon: @image-url("../../../assets/icons/open-workspace.svg"); accessible-name: GuiI18n.text("gui-open-server-workspace"); clicked => { root.open-workspace(item.id); } }
                }
            }
        }
        card-area := TouchArea { width: parent.width - 250px; clicked => { root.choose(item.id); } double-clicked => { root.open-workspace(item.id); } }
        ContextMenuArea {
            Menu {
                MenuItem { title: GuiI18n.text("gui-open-server-workspace"); activated => { root.open-workspace(item.id); } }
                MenuItem { title: item.can-start ? GuiI18n.text("gui-start") : GuiI18n.text("gui-stop"); enabled: item.can-start || item.can-stop; activated => { root.primary(item.id); } }
                MenuItem { title: GuiI18n.text("gui-restart"); enabled: item.can-restart; activated => { root.restart(item.id); } }
                MenuItem { title: GuiI18n.text("gui-open-folder"); activated => { root.open-folder(item.id); } }
                MenuSeparator { }
                MenuItem { title: GuiI18n.text("gui-force-kill"); enabled: item.can-kill; activated => { root.kill(item.id, item.name); } }
                MenuItem { title: GuiI18n.text("gui-delete-server"); activated => { root.delete(item.id, item.name); } }
            }
        }
    }

    component InspectorLabel inherits BodyText { color: Theme.text-muted; width: 105px; }

    component InstanceInspector inherits Rectangle {
        in property <InstanceRow> item;
        in property <bool> has-selection;
        callback launch(string);
        callback connect(string, string);
        callback open-folder(string);
        callback workspace(string);
        callback create-server(string, string);
        callback diagnostics(string);
        background: Theme.surface;
        border-width: 1px;
        border-color: Theme.border;
        if !has-selection : EmptyState { title: GuiI18n.text("gui-no-instance-selected"); message: GuiI18n.text("gui-select-instance"); }
        if has-selection : VerticalLayout {
            alignment: start; padding: 22px; spacing: 14px;
            HorizontalLayout {
                spacing: 12px;
                LoaderGlyph { kind: item.loader-kind; }
                VerticalLayout {
                    spacing: 4px;
                    Heading { text: item.name; font-size: Theme.item-title-size; }
                    BodyText { text: GuiI18n.values("gui-target-summary", item.minecraft, item.loader); color: Theme.text-muted; }
                }
            }
            Divider { }
            Eyebrow { text: GuiI18n.text("gui-overview"); }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-status"); } BodyText { text: item.needs-attention ? GuiI18n.text("gui-needs-attention") : item.state; color: item.needs-attention ? Theme.warning : item.state-kind == "running" ? Theme.success : item.state-kind == "starting" ? Theme.warning : Theme.text-secondary; } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-source"); } BodyText { text: item.source; } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-mods"); } BodyText { text: GuiI18n.count("gui-managed-count", item.mods); } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-storage"); } BodyText { text: item.storage; } }
            BodyText { text: item.path; color: Theme.text-muted; overflow: elide; }
            Divider { }
            Eyebrow { text: GuiI18n.text("gui-actions"); }
            if item.can-launch : AppButton { text: GuiI18n.text("gui-launch"); tone: "primary"; clicked => { root.launch(item.id); } }
            if item.connect-server-id != "" : AppButton { text: GuiI18n.value("gui-launch-connect", item.connect-server-name); enabled: item.can-connect; clicked => { root.connect(item.id, item.connect-server-id); } }
            HorizontalLayout { spacing: 8px;
                AppButton { text: GuiI18n.text("gui-open-workspace"); clicked => { root.workspace(item.id); } }
                AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-folder(item.id); } }
            }
            AppButton { text: GuiI18n.text("gui-create-linked-server"); clicked => { root.create-server(item.id, item.name); } }
            Divider { }
            Eyebrow { text: GuiI18n.text("gui-diagnostics"); }
            AppButton { text: GuiI18n.text("gui-view-logs-crash-reports"); clicked => { root.diagnostics(item.id); } }
        }
    }

    export component AppWindow inherits Window {
        title: "Ephervex";
        icon: root.logo;
        preferred-width: 1480px;
        preferred-height: 900px;
        min-width: 980px;
        min-height: 680px;
        background: Theme.background;
        default-font-size: Theme.body-size;

        in property <image> logo: @image-url("../../../assets/logo.png");
        in-out property <int> page: 0;
        in-out property <[InstanceRow]> instances;
        in-out property <[ServerRow]> servers;
        in-out property <[ModRow]> managed-mods;
        in-out property <[ModSearchRow]> mod-results;
        in-out property <[DiagnosticRow]> diagnostics;
        in-out property <[ActivityRow]> activities;
        in-out property <[AccountRow]> accounts;
        in-out property <[JavaRow]> java-runtimes;
        in-out property <[PreferenceRow]> preferences;
        in-out property <[PendingEntityRow]> pending-instances;
        in-out property <[PendingEntityRow]> pending-servers;
        in-out property <[string]> console-lines;
        in-out property <string> console-text;
        in-out property <string> selected-instance-id;
        in-out property <string> selected-server-id;
        in-out property <int> selected-instance-index: -1;
        in-out property <int> selected-server-index: -1;
        in-out property <string> instance-filter: "all";
        in-out property <string> instance-query;
        in-out property <string> server-query;
        in-out property <string> mod-query;
        in-out property <string> diagnostic-query;
        in-out property <string> diagnostic-content;
        in-out property <string> diagnostic-path;
        in-out property <string> diagnostic-title;
        in-out property <string> selected-diagnostic-name;
        in-out property <string> selected-diagnostic-scope;
        in-out property <string> selected-diagnostic-id;
        in-out property <string> selected-diagnostic-kind;
        in-out property <string> workspace: "";
        in-out property <string> workspace-tab: "Overview";
        in-out property <string> modal: "";
        in-out property <string> modal-error;
        in-out property <string> status-message: GuiI18n.text("gui-ready");
        in-out property <bool> loading: true;
        in-out property <int> active-operations;
        in-out property <string> storage-root;
        in-out property <string> app-version;
        in-out property <string> license-text;
        in-out property <string> toast-one-text;
        in-out property <bool> toast-one-error;
        in-out property <string> toast-two-text;
        in-out property <bool> toast-two-error;
        in-out property <string> toast-three-text;
        in-out property <bool> toast-three-error;
        in-out property <string> pending-action-id;
        in-out property <string> pending-action-name;
        in-out property <bool> show-inspector: true;
        in-out property <bool> compact-layout;
        in-out property <string> mod-plan-instance;
        in-out property <string> mod-plan-project;
        in-out property <string> mod-plan-title;
        in-out property <string> mod-plan-version;
        in-out property <int> mod-plan-install-count;
        in-out property <int> mod-plan-dependency-count;
        in-out property <int> mod-plan-optional-count;
        in-out property <int> mod-plan-existing-count;
        in-out property <int> mod-plan-warning-count;

        // Dialog form state.
        in-out property <string> create-name;
        in-out property <[string]> minecraft-releases;
        in-out property <[string]> create-loader-versions;
        in-out property <[string]> create-server-loader-versions;
        in-out property <string> create-minecraft: "1.21.1";
        in-out property <string> create-loader: "Vanilla";
        in-out property <string> create-loader-version;
        in-out property <bool> create-persistent: false;
        in-out property <string> import-path;
        in-out property <string> import-name;
        in-out property <string> import-analysis;
        in-out property <string> create-server-client-id;
        in-out property <string> create-server-name;
        in-out property <string> create-server-minecraft: "1.21.1";
        in-out property <string> create-server-loader: "Vanilla";
        in-out property <string> create-server-loader-version;
        in-out property <bool> create-server-persistent: false;
        in-out property <string> console-command;
        in-out property <string> property-motd;
        in-out property <string> property-max-players: "20";
        in-out property <string> property-difficulty: "normal";
        in-out property <string> property-gamemode: "survival";
        in-out property <bool> property-allow-flight;
        in-out property <bool> property-pvp: true;
        in-out property <bool> property-allowlist;
        in-out property <bool> property-online-mode;
        in-out property <string> property-access: "Localhost";
        in-out property <string> property-address;
        in-out property <string> property-port: "25565";
        in-out property <string> property-port-error;
        in-out property <string> property-address-error;
        in-out property <string> property-max-players-error;
        in-out property <bool> eula-accepted;
        in-out property <string> eula-status;
        in-out property <string> offline-account-name;
        in-out property <string> microsoft-client-id;
        in-out property <string> effective-microsoft-client-id;
        in-out property <bool> microsoft-client-id-is-custom;
        in-out property <string> user-agent;
        in-out property <string> effective-user-agent;
        in-out property <bool> user-agent-is-custom;
        in-out property <string> persistent-root;
        in-out property <string> default-persistent-root;
        in-out property <bool> persistent-root-is-custom;
        in-out property <int> application-text-scale: 100;
        in-out property <int> technical-text-size: 12;
        in-out property <string> java-memory-min: "512";
        in-out property <string> java-memory-max: "4096";
        in-out property <string> java-preferred-path;
        in-out property <bool> java-auto-detect: true;
        in-out property <bool> java-auto-download: true;

        callback refresh();
        callback request-version-catalog();
        callback warm-language-catalog(string);
        callback request-loader-catalog(string, string, string);
        callback select-instance(string);
        callback select-server(string);
        callback launch-client(string);
        callback launch-and-connect(string, string);
        callback open-client-folder(string);
        callback open-server-folder(string);
        callback duplicate-client(string);
        callback delete-client(string, bool);
        callback delete-server(string, bool);
        callback load-instance-workspace(string);
        callback search-mods(string, string);
        callback review-mod(string, string);
        callback install-mod(string, string);
        callback submit-create-instance(string, string, string, string, bool);
        callback choose-import-file();
        callback submit-import(string, string, bool);
        callback submit-create-server(string, string, string, string, string, bool);
        callback server-start(string);
        callback server-stop(string);
        callback server-restart(string);
        callback server-kill(string);
        callback server-console(string, string);
        callback load-server-workspace(string);
        callback save-server-properties(string, string, string, string, string, bool, bool, bool, bool, string, string, string);
        callback validate-server-properties(string, string, string, string);
        callback accept-eula();
        callback revoke-eula();
        callback open-eula-reference();
        callback load-diagnostics(string, string, string);
        callback open-diagnostic(string, string, string, string);
        callback search-diagnostic(string, string, string, string, string);
        callback refresh-diagnostic(string, string, string, string);
        callback open-diagnostic-folder(string, string, string);
        callback copy-text(string);
        callback add-offline-account(string);
        callback set-default-account(string);
        callback remove-account(string);
        callback begin-microsoft-login();
        callback save-api-settings(string, string);
        callback reset-microsoft-client-id();
        callback reset-user-agent();
        callback save-storage-settings(string);
        callback choose-storage-root();
        callback reset-storage-root();
        callback save-display-settings(int, int);
        callback refresh-java();
        callback save-java-settings(bool, bool, string, string, string);
        callback save-preference(string, string);
        callback reset-preference(string);
        callback cancel-operation(string);
        callback present-toast(string, bool);
        callback queue-toast(string, bool);
        callback toast-slot-available();

        present-toast(message, error) => {
            if root.toast-one-text == "" {
                root.toast-one-error = error;
                root.toast-one-text = message;
            } else if root.toast-two-text == "" {
                root.toast-two-error = error;
                root.toast-two-text = message;
            } else if root.toast-three-text == "" {
                root.toast-three-error = error;
                root.toast-three-text = message;
            } else {
                root.queue-toast(message, error);
            }
        }

        changed create-minecraft => {
            if root.create-minecraft != "" {
                root.warm-language-catalog(root.create-minecraft);
            }
        }
        changed create-server-minecraft => {
            if root.create-server-minecraft != "" {
                root.warm-language-catalog(root.create-server-minecraft);
            }
        }

        VerticalLayout {
            spacing: 0px;
            min-width: 980px;
            preferred-width: 1480px;
            min-height: 680px;
            preferred-height: 900px;
            // Application header.
            Rectangle {
                min-height: 68px;
                max-height: 68px;
                background: #11171a;
                border-width: 1px;
                border-color: Theme.border;
                HorizontalLayout {
                    alignment: center; padding-left: 20px; padding-right: 20px; spacing: 14px;
                    Image { source: root.logo; width: 36px; height: 36px; image-fit: contain; accessible-label: GuiI18n.text("gui-logo-accessible"); }
                    Heading { text: "Ephervex"; font-size: Theme.brand-size; width: 180px; }
                    Rectangle { horizontal-stretch: 1; }
                    if root.page == 0 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-import-modpack"); clicked => { root.modal = "import"; } }
                    if root.page == 0 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-new-instance"); tone: "primary"; min-width: 148px; clicked => { root.modal-error = ""; root.request-version-catalog(); root.warm-language-catalog(root.create-minecraft); root.modal = "create-instance"; } }
                    if root.page == 1 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-new-server"); tone: "primary"; min-width: 130px; clicked => { root.create-server-client-id = ""; root.create-server-name = ""; root.modal-error = ""; root.request-version-catalog(); root.warm-language-catalog(root.create-server-minecraft); root.modal = "create-server"; } }
                    IconButton { icon: @image-url("../../../assets/icons/refresh.svg"); accessible-name: GuiI18n.text("gui-refresh"); clicked => { root.refresh(); } }
                }
            }

            HorizontalLayout {
                spacing: 0px;
                // Primary navigation.
                Rectangle {
                    width: 218px;
                    background: #11171a;
                    border-width: 1px;
                    border-color: Theme.border;
                    VerticalLayout {
                        padding: 14px; spacing: 8px;
                        NavItem { icon: @image-url("../../../assets/icons/nav-instances.svg"); label: GuiI18n.text("gui-instances"); selected: root.page == 0; clicked => { root.page = 0; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-servers.svg"); label: GuiI18n.text("gui-servers"); selected: root.page == 1; clicked => { root.page = 1; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-activity.svg"); label: GuiI18n.text("gui-activity"); selected: root.page == 3; clicked => { root.page = 3; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-settings.svg"); label: GuiI18n.text("gui-settings"); selected: root.page == 4; clicked => { root.page = 4; root.workspace = ""; root.workspace-tab = "General"; } }
                        Rectangle { vertical-stretch: 1; }
                        BodyText { text: GuiI18n.text("gui-tagline"); color: Theme.text-muted; font-size: Theme.caption-size; wrap: word-wrap; }
                    }
                }

                // Instances list and inspector.
                if root.page == 0 && root.workspace == "" : HorizontalLayout {
                    spacing: 0px;
                    horizontal-stretch: 1;
                    Rectangle {
                        horizontal-stretch: 1;
                        background: Theme.background;
                        VerticalLayout {
                            padding: 24px; spacing: 18px;
                            HorizontalLayout {
                                PageIntro { title: GuiI18n.text("gui-instances"); subtitle: GuiI18n.text("gui-instances-detail"); }
                                BodyText { text: GuiI18n.count("gui-client-instances-count", root.instances.length); color: Theme.text-muted; }
                            }
                            Rectangle {
                                height: 58px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                HorizontalLayout {
                                    padding-left: 10px; padding-right: 10px; spacing: 8px;
                                    AppButton { text: GuiI18n.text("gui-all"); tone: root.instance-filter == "all" ? "primary" : "secondary"; min-width: 70px; clicked => { root.instance-filter = "all"; root.refresh(); } }
                                    AppButton { text: GuiI18n.text("gui-temporary"); tone: root.instance-filter == "temporary" ? "primary" : "secondary"; clicked => { root.instance-filter = "temporary"; root.refresh(); } }
                                    AppButton { text: GuiI18n.text("gui-persistent"); tone: root.instance-filter == "persistent" ? "primary" : "secondary"; clicked => { root.instance-filter = "persistent"; root.refresh(); } }
                                    Rectangle { horizontal-stretch: 1; }
                                    TextField { text <=> root.instance-query; placeholder: GuiI18n.text("gui-filter-instances"); input-type: InputType.search; width: 290px; accepted => { root.refresh(); } }
                                }
                            }
                            if root.loading : VerticalLayout { alignment: center; Spinner { width: 38px; height: 38px; } BodyText { text: GuiI18n.text("gui-loading-instances"); horizontal-alignment: center; } }
                            if !root.loading && root.instances.length == 0 && root.pending-instances.length == 0 : EmptyState {
                                title: GuiI18n.text("gui-no-client-instances");
                                message: GuiI18n.text("gui-no-client-instances-detail");
                                primary-text: GuiI18n.text("gui-modal-new-instance"); secondary-text: GuiI18n.text("gui-import-modpack");
                                primary => { root.modal-error = ""; root.request-version-catalog(); root.warm-language-catalog(root.create-minecraft); root.modal = "create-instance"; }
                                secondary => { root.modal = "import"; }
                            }
                            if !root.loading && (root.instances.length > 0 || root.pending-instances.length > 0) : ScrollView {
                                viewport-width: self.width;
                                VerticalLayout {
                                    spacing: 12px;
                                    for pending in root.pending-instances : PendingCard { item: pending; }
                                    for item[index] in root.instances : InstanceCard {
                                        visible: root.instance-filter == "all" || item.storage-kind == root.instance-filter;
                                        item: item;
                                        selected: item.id == root.selected-instance-id;
                                        choose(id) => { root.selected-instance-id = id; root.select-instance(id); }
                                        launch(id) => { root.launch-client(id); }
                                        open-workspace(id) => { root.selected-instance-id = id; root.selected-instance-index = index; root.select-instance(id); root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); }
                                        open-folder(id) => { root.open-client-folder(id); }
                                        create-server(id, name) => { root.create-server-client-id = id; root.create-server-name = name + " Server"; root.modal = "create-server"; }
                                        duplicate(id) => { root.duplicate-client(id); }
                                        delete(id, name) => { root.pending-action-id = id; root.pending-action-name = name; root.modal = "delete-instance"; }
                                    }
                                }
                            }
                        }
                    }
                    if root.show-inspector && root.selected-instance-index >= 0 : InstanceInspector {
                        width: 360px;
                        has-selection: root.selected-instance-index >= 0;
                        item: root.selected-instance-index >= 0 ? root.instances[root.selected-instance-index] : {};
                        launch(id) => { root.launch-client(id); }
                        connect(client, server) => { root.launch-and-connect(client, server); }
                        open-folder(id) => { root.open-client-folder(id); }
                        workspace(id) => { root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); }
                        create-server(id, name) => { root.create-server-client-id = id; root.create-server-name = name + " Server"; root.modal = "create-server"; }
                        diagnostics(id) => { root.page = 2; root.load-diagnostics("client", id, "log"); }
                    }
                }

                if root.page == 0 && root.workspace == "instance" : Rectangle {
                    horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout {
                        padding: 22px; spacing: 14px;
                        HorizontalLayout { spacing: 12px; AppButton { text: GuiI18n.text("gui-back-instances"); clicked => { root.workspace = ""; } } Heading { text: root.selected-instance-index >= 0 ? root.instances[root.selected-instance-index].name : GuiI18n.text("gui-instance"); font-size: Theme.workspace-title-size; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-launch"); tone: "primary"; enabled: root.selected-instance-index >= 0 && root.instances[root.selected-instance-index].can-launch; clicked => { root.launch-client(root.selected-instance-id); } } }
                        HorizontalLayout {
                            spacing: 4px;
                            WorkspaceTab { text: GuiI18n.text("gui-instance-tab-overview"); selected: root.workspace-tab == "Overview"; clicked => { root.workspace-tab = "Overview"; } }
                            WorkspaceTab { text: GuiI18n.text("gui-instance-tab-mods"); selected: root.workspace-tab == "Mods"; clicked => { root.workspace-tab = "Mods"; } }
                            WorkspaceTab { text: GuiI18n.text("gui-instance-tab-minecraft-settings"); selected: root.workspace-tab == "Minecraft Settings"; clicked => { root.workspace-tab = "Minecraft Settings"; } }
                            WorkspaceTab { text: GuiI18n.text("gui-instance-tab-logs"); selected: false; clicked => { root.page = 2; root.load-diagnostics("client", root.selected-instance-id, "log"); } }
                            Rectangle { horizontal-stretch: 1; }
                        }
                        Divider { }
                        if root.workspace-tab == "Overview" : ScrollView {
                            VerticalLayout { alignment: start; width: min(parent.width, 980px); spacing: 12px;
                                Heading { text: GuiI18n.text("gui-reproduction-environment"); }
                                BodyText { text: GuiI18n.text("gui-reproduction-environment-detail"); }
                                Rectangle { height: 126px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    HorizontalLayout { padding: 18px; spacing: 18px; LoaderGlyph { kind: root.instances[root.selected-instance-index].loader-kind; } VerticalLayout { Heading { text: root.instances[root.selected-instance-index].name; } BodyText { text: GuiI18n.values("gui-target-summary", root.instances[root.selected-instance-index].minecraft, root.instances[root.selected-instance-index].loader); } BodyText { text: root.instances[root.selected-instance-index].storage + " | " + root.instances[root.selected-instance-index].source; } BodyText { text: root.instances[root.selected-instance-index].path; color: Theme.text-muted; } } }
                                }
                                BodyText { text: root.instances[root.selected-instance-index].storage-kind == "temporary" ? GuiI18n.text("gui-temporary-lifecycle") : GuiI18n.text("gui-persistent-lifecycle"); color: Theme.text-muted; wrap: word-wrap; }
                            }
                        }
                        if root.workspace-tab == "Mods" : HorizontalLayout {
                            spacing: 16px;
                            Rectangle { width: 42%; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                VerticalLayout { padding: 16px; spacing: 10px; Heading { text: GuiI18n.text("gui-managed-mods"); } BodyText { text: GuiI18n.text("gui-managed-mods-detail"); wrap: word-wrap; }
                                    if root.managed-mods.length == 0 : EmptyState { title: GuiI18n.text("gui-no-managed-mods"); message: GuiI18n.text("gui-no-managed-mods-detail"); }
                                    ScrollView { VerticalLayout { spacing: 8px; for mod in root.managed-mods : Rectangle { height: 70px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; VerticalLayout { padding: 10px; Heading { text: mod.title; font-size: Theme.row-heading-size; } BodyText { text: mod.version + " | " + mod.provider + " | " + mod.origin; } } } } }
                                }
                            }
                            Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                VerticalLayout { padding: 16px; spacing: 10px; Heading { text: GuiI18n.text("gui-add-modrinth-mods"); } BodyText { text: GuiI18n.text("gui-modrinth-compatibility"); wrap: word-wrap; }
                                    HorizontalLayout { TextField { text <=> root.mod-query; placeholder: GuiI18n.text("gui-search-compatible-mods"); input-type: InputType.search; horizontal-stretch: 1; accepted => { root.search-mods(root.selected-instance-id, root.mod-query); } } AppButton { text: GuiI18n.text("gui-search"); tone: "primary"; clicked => { root.search-mods(root.selected-instance-id, root.mod-query); } } }
                                    ScrollView { VerticalLayout { spacing: 8px; for mod in root.mod-results : Rectangle { height: 92px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; HorizontalLayout { padding: 10px; VerticalLayout { Heading { text: mod.title; font-size: Theme.row-heading-size; } BodyText { text: mod.description; wrap: word-wrap; } BodyText { text: GuiI18n.values("gui-mod-result-meta", mod.author, mod.downloads); color: Theme.text-muted; } } AppButton { text: GuiI18n.text("gui-review"); clicked => { root.review-mod(root.selected-instance-id, mod.project-id); } } } } } }
                                }
                            }
                        }
                        if root.workspace-tab == "Minecraft Settings" : EmptyState { title: GuiI18n.text("gui-instance-settings-unavailable"); message: GuiI18n.text("gui-instance-settings-unavailable-detail"); primary-text: GuiI18n.text("gui-open-global-defaults"); primary => { root.page = 4; root.workspace = ""; root.workspace-tab = "Minecraft"; } }
                    }
                }

                // Servers.
                if root.page == 1 && root.workspace == "" : HorizontalLayout {
                    spacing: 0px;
                    horizontal-stretch: 1;
                    Rectangle { horizontal-stretch: 1; background: Theme.background;
                        VerticalLayout { padding: 24px; spacing: 18px;
                            HorizontalLayout {
                                PageIntro { title: GuiI18n.text("gui-servers"); subtitle: GuiI18n.text("gui-servers-detail"); }
                                TextField { text <=> root.server-query; placeholder: GuiI18n.text("gui-filter-servers"); input-type: InputType.search; width: 290px; accepted => { root.refresh(); } }
                            }
                            if root.servers.length == 0 && root.pending-servers.length == 0 : EmptyState { title: GuiI18n.text("gui-no-servers"); message: GuiI18n.text("gui-no-servers-detail"); primary-text: GuiI18n.text("gui-modal-new-server"); primary => { root.create-server-client-id = ""; root.create-server-name = ""; root.modal-error = ""; root.request-version-catalog(); root.warm-language-catalog(root.create-server-minecraft); root.modal = "create-server"; } }
                            if root.servers.length > 0 || root.pending-servers.length > 0 : ScrollView { viewport-width: self.width; VerticalLayout { spacing: 12px; for pending in root.pending-servers : PendingCard { item: pending; } for item[index] in root.servers : ServerCard { item: item; selected: item.id == root.selected-server-id; choose(id) => { root.selected-server-id = id; root.selected-server-index = index; root.select-server(id); } primary(id) => { if item.can-start { root.server-start(id); } else { root.server-stop(id); } } open-workspace(id) => { root.selected-server-id = id; root.selected-server-index = index; root.select-server(id); root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(id); } open-folder(id) => { root.open-server-folder(id); } restart(id) => { root.server-restart(id); } kill(id, name) => { root.selected-server-id = id; root.pending-action-name = name; root.modal = "kill"; } delete(id, name) => { root.pending-action-id = id; root.pending-action-name = name; root.modal = "delete-server"; } } } }
                        }
                    }
                    if root.show-inspector && root.selected-server-index >= 0 : Rectangle { width: 360px; background: Theme.surface; border-width: 1px; border-color: Theme.border;
                        if root.selected-server-index < 0 : EmptyState { title: GuiI18n.text("gui-no-server-selected"); message: GuiI18n.text("gui-select-server"); }
                        if root.selected-server-index >= 0 : VerticalLayout { alignment: start; padding: 22px; spacing: 12px;
                            HorizontalLayout {
                                spacing: 12px;
                                LoaderGlyph { kind: root.servers[root.selected-server-index].loader-kind; server: true; }
                                VerticalLayout { spacing: 4px;
                                    Heading { text: root.servers[root.selected-server-index].name; }
                                    Pill { text: root.servers[root.selected-server-index].state; tone-color: root.servers[root.selected-server-index].state-kind == "running" ? Theme.success : Theme.text-secondary; }
                                }
                            }
                            Divider { }
                            Eyebrow { text: GuiI18n.text("gui-server-run"); }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-target"); } BodyText { text: root.servers[root.selected-server-index].minecraft + " | " + root.servers[root.selected-server-index].loader; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-relation"); } BodyText { text: root.servers[root.selected-server-index].relationship; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-address"); } BodyText { text: root.servers[root.selected-server-index].address; } }
                            Divider { }
                            Eyebrow { text: GuiI18n.text("gui-actions"); }
                            if root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop : AppButton { text: root.servers[root.selected-server-index].can-start ? GuiI18n.text("gui-start") : GuiI18n.text("gui-stop"); tone: "primary"; clicked => { if root.servers[root.selected-server-index].can-start { root.server-start(root.selected-server-id); } else { root.server-stop(root.selected-server-id); } } }
                            HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-open-server-workspace"); clicked => { root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } }
                            if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: GuiI18n.text("gui-launch-client-connect"); clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } }
                            HorizontalLayout { spacing: 8px;
                                AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(root.selected-server-id); } }
                                AppButton { text: GuiI18n.text("gui-view-logs"); clicked => { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } }
                            }
                            Divider { }
                            AppButton { text: GuiI18n.text("gui-force-kill"); tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } }
                        }
                    }
                }

                if root.page == 1 && root.workspace == "server" : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 22px; spacing: 14px;
                        HorizontalLayout { spacing: 12px; AppButton { text: GuiI18n.text("gui-back-servers"); clicked => { root.workspace = ""; } } Heading { text: root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : GuiI18n.text("gui-server"); font-size: Theme.workspace-title-size; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-stop ? GuiI18n.text("gui-stop") : GuiI18n.text("gui-start"); tone: "primary"; enabled: root.selected-server-index >= 0 && (root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop); clicked => { if root.servers[root.selected-server-index].can-stop { root.server-stop(root.selected-server-id); } else { root.server-start(root.selected-server-id); } } } }
                        HorizontalLayout { spacing: 8px;
                            WorkspaceTab { text: GuiI18n.text("gui-instance-tab-overview"); selected: root.workspace-tab == "Overview"; clicked => { root.workspace-tab = "Overview"; } }
                            WorkspaceTab { text: GuiI18n.text("gui-server-tab-console"); selected: root.workspace-tab == "Console"; clicked => { root.workspace-tab = "Console"; } }
                            WorkspaceTab { text: GuiI18n.text("gui-server-tab-properties"); selected: root.workspace-tab == "Properties"; clicked => { root.workspace-tab = "Properties"; } }
                            WorkspaceTab { text: GuiI18n.text("gui-instance-tab-logs"); selected: false; clicked => { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } }
                            Rectangle { horizontal-stretch: 1; }
                        }
                        Divider { }
                        if root.workspace-tab == "Overview" : ScrollView { VerticalLayout { alignment: start; width: min(parent.width, 980px); spacing: 12px; Heading { text: GuiI18n.text("gui-dedicated-server"); } BodyText { text: root.servers[root.selected-server-index].state-detail; } Rectangle { height: 160px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 18px; spacing: 7px;
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-exact-target"); } BodyText { text: GuiI18n.values("gui-target-summary", root.servers[root.selected-server-index].minecraft, root.servers[root.selected-server-index].loader); } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-runtime"); } BodyText { text: root.servers[root.selected-server-index].state + " | " + root.servers[root.selected-server-index].state-detail; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-network"); } BodyText { text: root.servers[root.selected-server-index].address; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-storage"); } BodyText { text: root.servers[root.selected-server-index].storage; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-relationship"); } BodyText { text: root.servers[root.selected-server-index].relationship; } }
                        } } HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-view-logs"); clicked => { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } } if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: GuiI18n.text("gui-launch-client-connect"); tone: "primary"; clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } } } } }
                        if root.workspace-tab == "Console" : VerticalLayout { spacing: 10px;
                            HorizontalLayout { BodyText { text: GuiI18n.value("gui-bounded-output", root.servers[root.selected-server-index].state); color: root.servers[root.selected-server-index].can-send-command ? Theme.success : Theme.text-secondary; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-stop"); enabled: root.servers[root.selected-server-index].can-stop; clicked => { root.server-stop(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-kill"); tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } } }
                            SelectableText { vertical-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; text: root.console-text; font-size: root.technical-text-size * 1px; }
                            HorizontalLayout { TextField { text <=> root.console-command; placeholder: root.servers[root.selected-server-index].can-send-command ? GuiI18n.text("gui-enter-server-command") : GuiI18n.text("gui-start-server-command"); enabled: root.servers[root.selected-server-index].can-send-command; horizontal-stretch: 1; accepted => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } AppButton { text: GuiI18n.text("gui-send"); tone: "primary"; enabled: root.servers[root.selected-server-index].can-send-command && root.console-command != ""; clicked => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } }
                        }
                        if root.workspace-tab == "Properties" : ScrollView { VerticalLayout { alignment: start; width: min(parent.width, 1100px); spacing: 14px;
                            Heading { text: GuiI18n.text("gui-network"); } BodyText { text: GuiI18n.text("gui-network-detail"); wrap: word-wrap; }
                            HorizontalLayout { spacing: 12px; VerticalLayout { FieldLabel { text: GuiI18n.text("gui-access"); } ComboBox { model: [GuiI18n.text("gui-access-localhost"), GuiI18n.text("gui-access-lan"), GuiI18n.text("gui-access-custom")]; current-index: root.property-access == "LAN" ? 1 : root.property-access == "Custom" ? 2 : 0; selected(value) => { root.property-access = value == GuiI18n.text("gui-access-lan") ? "LAN" : value == GuiI18n.text("gui-access-custom") ? "Custom" : "Localhost"; root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } width: 190px; } Rectangle { height: 14px; } } VerticalLayout { FieldLabel { text: GuiI18n.text("gui-port"); } TextField { text <=> root.property-port; placeholder: "25565"; input-type: InputType.number; invalid: root.property-port-error != ""; width: 150px; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } Rectangle { height: 14px; Text { text: root.property-port-error; visible: root.property-port-error != ""; color: Theme.danger; font-size: 11px; } } } VerticalLayout { horizontal-stretch: 1; FieldLabel { text: GuiI18n.text("gui-custom-bind-address"); } TextField { text <=> root.property-address; placeholder: "127.0.0.1 or ::1"; enabled: root.property-access == "Custom"; invalid: root.property-address-error != ""; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } Rectangle { height: 14px; Text { text: root.property-address-error; visible: root.property-address-error != ""; color: Theme.danger; font-size: 11px; } } } }
                            Rectangle { height: 32px; HorizontalLayout { spacing: 10px; LabeledToggle { label: GuiI18n.text("gui-online-mode"); checked <=> root.property-online-mode; } BodyText { visible: !root.property-online-mode && root.property-access != "Localhost"; text: GuiI18n.text("gui-network-warning"); color: Theme.warning; } } }
                            Divider { } Heading { text: GuiI18n.text("gui-gameplay"); }
                            HorizontalLayout { spacing: 12px; VerticalLayout { horizontal-stretch: 1; FieldLabel { text: GuiI18n.text("gui-message-of-the-day"); } TextField { text <=> root.property-motd; placeholder: GuiI18n.text("gui-default-motd"); } } VerticalLayout { width: 190px; FieldLabel { text: GuiI18n.text("gui-maximum-players"); } TextField { text <=> root.property-max-players; placeholder: "20"; input-type: InputType.number; invalid: root.property-max-players-error != ""; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } if root.property-max-players-error != "" : Text { text: root.property-max-players-error; color: Theme.danger; font-size: 11px; } } }
                            HorizontalLayout { spacing: 12px; VerticalLayout { Text { text: GuiI18n.text("gui-difficulty"); color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["peaceful", "easy", "normal", "hard"]; current-index: root.property-difficulty == "peaceful" ? 0 : root.property-difficulty == "easy" ? 1 : root.property-difficulty == "hard" ? 3 : 2; selected(value) => { root.property-difficulty = value; } width: 240px; } } VerticalLayout { Text { text: GuiI18n.text("gui-game-mode"); color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["survival", "creative", "adventure", "spectator"]; current-index: root.property-gamemode == "creative" ? 1 : root.property-gamemode == "adventure" ? 2 : root.property-gamemode == "spectator" ? 3 : 0; selected(value) => { root.property-gamemode = value; } width: 270px; } } }
                            HorizontalLayout { spacing: 14px; LabeledToggle { label: GuiI18n.text("gui-allow-flight"); checked <=> root.property-allow-flight; } LabeledToggle { label: GuiI18n.text("gui-pvp"); checked <=> root.property-pvp; } LabeledToggle { label: GuiI18n.text("gui-allowlist"); checked <=> root.property-allowlist; } Rectangle { horizontal-stretch: 1; } }
                            Rectangle { height: 84px; border-radius: 8px; border-width: 1px; border-color: root.eula-accepted ? Theme.success : Theme.warning; background: Theme.surface; HorizontalLayout { padding: 14px; VerticalLayout { Heading { text: GuiI18n.text("gui-server-eula"); font-size: 14px; } BodyText { text: root.eula-status; wrap: word-wrap; } } AppButton { text: root.eula-accepted ? GuiI18n.text("gui-eula-review") : GuiI18n.text("gui-eula-review-accept"); clicked => { root.modal = "eula"; } } } }
                            HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-changes"); tone: "primary"; enabled: root.servers[root.selected-server-index].state-kind == "stopped"; clicked => { root.save-server-properties(root.selected-server-id, root.property-motd, root.property-max-players, root.property-difficulty, root.property-gamemode, root.property-allow-flight, root.property-pvp, root.property-allowlist, root.property-online-mode, root.property-access, root.property-address, root.property-port); } } }
                        } }
                    }
                }

                // Logs and crash reports.
                if root.page == 2 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 20px; spacing: 12px;
                        if !root.compact-layout : HorizontalLayout { Heading { text: GuiI18n.text("gui-logs-crash-reports"); font-size: 25px; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-client"); clicked => { if root.selected-instance-id != "" { root.load-diagnostics("client", root.selected-instance-id, "log"); } } } AppButton { text: GuiI18n.text("gui-server-source"); clicked => { if root.selected-server-id != "" { root.load-diagnostics("server", root.selected-server-id, "log"); } } } AppButton { text: GuiI18n.text("gui-launcher"); tone: root.selected-diagnostic-scope == "launcher" ? "primary" : "secondary"; clicked => { root.load-diagnostics("launcher", "", "log"); } } }
                        if root.compact-layout : VerticalLayout { spacing: 8px; Heading { text: GuiI18n.text("gui-logs-crash-reports"); font-size: 25px; } HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-client"); clicked => { if root.selected-instance-id != "" { root.load-diagnostics("client", root.selected-instance-id, "log"); } } } AppButton { text: GuiI18n.text("gui-server-source"); clicked => { if root.selected-server-id != "" { root.load-diagnostics("server", root.selected-server-id, "log"); } } } AppButton { text: GuiI18n.text("gui-launcher"); tone: root.selected-diagnostic-scope == "launcher" ? "primary" : "secondary"; clicked => { root.load-diagnostics("launcher", "", "log"); } } Rectangle { horizontal-stretch: 1; } } }
                        HorizontalLayout { spacing: 12px;
                            Rectangle { width: 300px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 10px; spacing: 7px;
                                HorizontalLayout { BodyText { text: GuiI18n.count("gui-diagnostic-files-count", root.diagnostics.length); } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-refresh"); clicked => { root.load-diagnostics(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } }
                                if root.diagnostics.length == 0 : EmptyState { title: GuiI18n.text("gui-no-diagnostics"); message: GuiI18n.text("gui-no-diagnostics-detail"); }
                                ScrollView { VerticalLayout { spacing: 5px; for item in root.diagnostics : Rectangle { height: 76px; border-radius: 6px; border-width: item.name == root.selected-diagnostic-name ? 2px : 1px; border-color: item.name == root.selected-diagnostic-name ? Theme.accent : Theme.border; background: Theme.background; VerticalLayout { padding: 8px; Text { text: GuiI18n.value(item.kind == "crash-report" ? "gui-crash-name" : "gui-log-name", item.name); color: item.kind == "crash-report" ? Theme.warning : Theme.text; font-size: 13px; overflow: elide; } BodyText { text: item.entity + " | " + item.modified; font-size: 11px; } BodyText { text: item.summary != "" ? item.summary : item.size; color: Theme.text-muted; font-size: 11px; overflow: elide; } } TouchArea { clicked => { root.selected-diagnostic-name = item.name; root.selected-diagnostic-scope = item.scope-kind; root.selected-diagnostic-id = item.scope-id; root.selected-diagnostic-kind = item.kind; root.open-diagnostic(item.scope-kind, item.scope-id, item.kind, item.name); } } } } }
                            } }
                            Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: rgb(11, 14, 16);
                                VerticalLayout { spacing: 0px;
                                    Rectangle { height: root.compact-layout ? 96px : 52px; background: Theme.surface;
                                        if !root.compact-layout : HorizontalLayout { padding-left: 10px; padding-right: 10px; spacing: 8px; Heading { text: root.diagnostic-title != "" ? root.diagnostic-title : GuiI18n.text("gui-select-diagnostic"); font-size: 14px; } Rectangle { horizontal-stretch: 1; } TextField { text <=> root.diagnostic-query; placeholder: GuiI18n.text("gui-search-loaded-content"); input-type: InputType.search; width: 250px; accepted => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-find"); enabled: root.selected-diagnostic-name != ""; clicked => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-copy"); enabled: root.diagnostic-content != ""; clicked => { root.copy-text(root.diagnostic-content); } } AppButton { text: GuiI18n.text("gui-folder"); enabled: root.selected-diagnostic-name != ""; clicked => { root.open-diagnostic-folder(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } AppButton { text: GuiI18n.text("gui-refresh"); enabled: root.selected-diagnostic-name != ""; clicked => { root.refresh-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name); } } }
                                        if root.compact-layout : VerticalLayout { padding: 8px; spacing: 6px; HorizontalLayout { Heading { text: root.diagnostic-title != "" ? root.diagnostic-title : GuiI18n.text("gui-select-diagnostic"); font-size: 14px; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-copy"); enabled: root.diagnostic-content != ""; clicked => { root.copy-text(root.diagnostic-content); } } } HorizontalLayout { spacing: 8px; TextField { text <=> root.diagnostic-query; placeholder: GuiI18n.text("gui-search-loaded-content"); input-type: InputType.search; horizontal-stretch: 1; accepted => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-find"); enabled: root.selected-diagnostic-name != ""; clicked => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-folder"); enabled: root.selected-diagnostic-name != ""; clicked => { root.open-diagnostic-folder(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } AppButton { text: GuiI18n.text("gui-refresh"); enabled: root.selected-diagnostic-name != ""; clicked => { root.refresh-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name); } } } }
                                    }
                                    if root.diagnostic-content == "" : EmptyState { title: GuiI18n.text("gui-no-file-open"); message: GuiI18n.text("gui-no-file-open-detail"); }
                                    if root.diagnostic-content != "" : SelectableText { text: root.diagnostic-content; font-size: root.technical-text-size * 1px; }
                                    if root.diagnostic-path != "" : Rectangle { height: 30px; background: Theme.surface; HorizontalLayout { padding-left: 10px; BodyText { text: root.diagnostic-path; color: Theme.text-muted; overflow: elide; } AppButton { text: GuiI18n.text("gui-copy-path"); min-width: 88px; height: 28px; clicked => { root.copy-text(root.diagnostic-path); } } } }
                                }
                            }
                        }
                    }
                }

                if root.page == 3 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 24px; spacing: 18px;
                        PageIntro { title: GuiI18n.text("gui-activity"); subtitle: GuiI18n.text("gui-activity-detail"); }
                        if root.activities.length == 0 : EmptyState { title: GuiI18n.text("gui-no-recent-activity"); message: GuiI18n.text("gui-no-recent-activity-detail"); }
                        ScrollView { viewport-width: self.width; VerticalLayout { spacing: 10px; for item in root.activities : ActivityCard { item: item; cancel(id) => { root.cancel-operation(id); } } } }
                    }
                }

                // Settings.
                if root.page == 4 : HorizontalLayout { spacing: 0px; horizontal-stretch: 1;
                    Rectangle { width: 208px; background: Theme.surface; border-width: 1px; border-color: Theme.border; VerticalLayout { padding: 14px; spacing: 5px; Eyebrow { text: GuiI18n.text("gui-settings-heading"); }
                        Rectangle { height: 5px; }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-general"); selected: root.workspace-tab == "General"; clicked => { root.workspace-tab = "General"; } }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-minecraft"); selected: root.workspace-tab == "Minecraft"; clicked => { root.workspace-tab = "Minecraft"; } }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-java"); selected: root.workspace-tab == "Java"; clicked => { root.workspace-tab = "Java"; } }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-accounts"); selected: root.workspace-tab == "Accounts"; clicked => { root.workspace-tab = "Accounts"; } }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-providers"); selected: root.workspace-tab == "Providers & APIs"; clicked => { root.workspace-tab = "Providers & APIs"; } }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-storage"); selected: root.workspace-tab == "Storage"; clicked => { root.workspace-tab = "Storage"; } }
                        Rectangle { vertical-stretch: 1; }
                        Divider { }
                        SettingsNavItem { label: GuiI18n.text("gui-settings-about"); selected: root.workspace-tab == "About & Licenses"; clicked => { root.workspace-tab = "About & Licenses"; } }
                    } }
                    Rectangle { horizontal-stretch: 1; background: Theme.background; ScrollView { viewport-width: self.width; VerticalLayout { x: max(0px, (parent.width - self.width) / 2); alignment: start; width: min(parent.width, 1100px); padding: 30px; spacing: 20px;
                        PageIntro {
                            title: root.workspace-tab == "General" ? GuiI18n.text("gui-settings-general") : root.workspace-tab == "Minecraft" ? GuiI18n.text("gui-settings-minecraft") : root.workspace-tab == "Java" ? GuiI18n.text("gui-settings-java") : root.workspace-tab == "Accounts" ? GuiI18n.text("gui-settings-accounts") : root.workspace-tab == "Providers & APIs" ? GuiI18n.text("gui-settings-providers") : root.workspace-tab == "Storage" ? GuiI18n.text("gui-settings-storage") : GuiI18n.text("gui-settings-about");
                            subtitle: root.workspace-tab == "Minecraft" ? GuiI18n.text("gui-minecraft-defaults-detail") : root.workspace-tab == "Java" ? GuiI18n.text("gui-java-detail") : root.workspace-tab == "Accounts" ? GuiI18n.text("gui-accounts-detail") : root.workspace-tab == "Providers & APIs" ? GuiI18n.text("gui-provider-detail") : root.workspace-tab == "Storage" ? GuiI18n.text("gui-storage-detail") : root.workspace-tab == "About & Licenses" ? GuiI18n.text("gui-tagline") : GuiI18n.text("gui-settings-detail");
                        }
                        if root.workspace-tab == "General" : VerticalLayout { spacing: 14px;
                            SectionTitle { title: GuiI18n.text("gui-application-display"); detail: GuiI18n.text("gui-application-display-detail"); }
                            Rectangle { height: root.application-text-scale >= 125 ? 124px : root.application-text-scale >= 110 ? 112px : 104px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                HorizontalLayout { padding: 16px; spacing: 20px; alignment: center;
                                    VerticalLayout { horizontal-stretch: 1; spacing: 5px; Heading { text: GuiI18n.text("gui-application-text-size"); font-size: Theme.row-heading-size; } BodyText { text: GuiI18n.text("gui-application-text-size-detail"); wrap: word-wrap; } }
                                    ComboBox { width: 190px; model: [GuiI18n.text("gui-text-scale-90"), GuiI18n.text("gui-text-scale-100"), GuiI18n.text("gui-text-scale-110"), GuiI18n.text("gui-text-scale-125")]; current-index: root.application-text-scale == 90 ? 0 : root.application-text-scale == 110 ? 2 : root.application-text-scale == 125 ? 3 : 1; selected(value) => { root.application-text-scale = value == GuiI18n.text("gui-text-scale-90") ? 90 : value == GuiI18n.text("gui-text-scale-110") ? 110 : value == GuiI18n.text("gui-text-scale-125") ? 125 : 100; } }
                                }
                            }
                            Rectangle { height: root.application-text-scale >= 125 ? 124px : root.application-text-scale >= 110 ? 112px : 104px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                HorizontalLayout { padding: 16px; spacing: 20px; alignment: center;
                                    VerticalLayout { horizontal-stretch: 1; spacing: 5px; Heading { text: GuiI18n.text("gui-technical-text-size"); font-size: Theme.row-heading-size; } BodyText { text: GuiI18n.text("gui-technical-text-size-detail"); wrap: word-wrap; } }
                                    ComboBox { width: 190px; model: [GuiI18n.text("gui-technical-size-11"), GuiI18n.text("gui-technical-size-12"), GuiI18n.text("gui-technical-size-14"), GuiI18n.text("gui-technical-size-16")]; current-index: root.technical-text-size == 11 ? 0 : root.technical-text-size == 14 ? 2 : root.technical-text-size == 16 ? 3 : 1; selected(value) => { root.technical-text-size = value == GuiI18n.text("gui-technical-size-11") ? 11 : value == GuiI18n.text("gui-technical-size-14") ? 14 : value == GuiI18n.text("gui-technical-size-16") ? 16 : 12; } }
                                }
                            }
                            HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-display-settings"); tone: "primary"; clicked => { root.save-display-settings(root.application-text-scale, root.technical-text-size); } } }
                        }
                        if root.workspace-tab == "Minecraft" : VerticalLayout { spacing: 12px;
                            SectionTitle { title: GuiI18n.text("gui-display-gameplay-defaults"); detail: ""; }
                            for pref in root.preferences : PreferenceCard { item: pref; compact: root.compact-layout; save(key, value) => { root.save-preference(key, value); } reset(key) => { root.reset-preference(key); } }
                            BodyText { text: GuiI18n.text("gui-options-unavailable-detail"); color: Theme.text-muted; wrap: word-wrap; }
                        }
                        if root.workspace-tab == "Java" : VerticalLayout { spacing: 12px;
                            VerticalLayout { spacing: 6px;
                                LabeledToggle { width: min(parent.width, 440px); label: GuiI18n.text("gui-java-detect"); checked <=> root.java-auto-detect; }
                                LabeledToggle { width: min(parent.width, 440px); label: GuiI18n.text("gui-java-download"); checked <=> root.java-auto-download; }
                            }
                            HorizontalLayout { spacing: 12px; VerticalLayout { FieldLabel { text: GuiI18n.text("gui-java-memory-min"); } TextField { text <=> root.java-memory-min; input-type: InputType.number; width: 190px; } } VerticalLayout { FieldLabel { text: GuiI18n.text("gui-java-memory-max"); } TextField { text <=> root.java-memory-max; input-type: InputType.number; width: 190px; } } }
                            Text { text: GuiI18n.text("gui-java-preferred"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.java-preferred-path; placeholder: "/usr/bin/java"; }
                            HorizontalLayout { AppButton { text: GuiI18n.text("gui-java-refresh"); clicked => { root.refresh-java(); } } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-java-save"); tone: "primary"; clicked => { root.save-java-settings(root.java-auto-detect, root.java-auto-download, root.java-memory-min, root.java-memory-max, root.java-preferred-path); } } }
                            for java in root.java-runtimes : Rectangle { height: 75px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; VerticalLayout { Heading { text: java.version + " | " + java.vendor; font-size: 14px; } BodyText { text: java.path; overflow: elide; } } Pill { text: java.origin; tone-color: Theme.accent-secondary; } } }
                        }
                        if root.workspace-tab == "Accounts" : VerticalLayout { spacing: 12px;
                            TextField { text <=> root.offline-account-name; placeholder: GuiI18n.text("gui-offline-account-placeholder"); }
                            HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-add-offline-account"); width: 180px; clicked => { root.add-offline-account(root.offline-account-name); } } AppButton { text: GuiI18n.text("gui-microsoft-sign-in"); width: 205px; tone: "primary"; clicked => { root.begin-microsoft-login(); } } Rectangle { horizontal-stretch: 1; } }
                            if root.accounts.length == 0 : EmptyState { title: GuiI18n.text("gui-no-accounts"); message: GuiI18n.text("gui-no-accounts-detail"); }
                            for account in root.accounts : Rectangle { height: 78px; border-radius: 8px; border-width: account.is-default ? 2px : 1px; border-color: account.is-default ? Theme.accent : Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; spacing: 8px; VerticalLayout { Heading { text: account.name; font-size: 14px; } BodyText { text: account.kind + " | " + account.credential-state; } } if account.is-default : Pill { text: GuiI18n.text("gui-default"); tone-color: Theme.accent; } if !account.is-default : AppButton { text: GuiI18n.text("gui-make-default"); width: 130px; clicked => { root.set-default-account(account.id); } } AppButton { text: GuiI18n.text("gui-remove"); width: 110px; tone: "danger"; clicked => { root.remove-account(account.id); } } } }
                        }
                        if root.workspace-tab == "Providers & APIs" : VerticalLayout { spacing: 14px;
                            SectionTitle { title: GuiI18n.text("gui-provider-identifiers"); detail: GuiI18n.text("gui-provider-identifiers-detail"); }
                            Rectangle { height: 146px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 16px; spacing: 8px;
                                HorizontalLayout { Heading { text: GuiI18n.text("gui-microsoft-client-id"); font-size: Theme.row-heading-size; } Rectangle { horizontal-stretch: 1; } Pill { text: root.microsoft-client-id-is-custom ? GuiI18n.text("gui-override") : GuiI18n.text("gui-default"); tone-color: root.microsoft-client-id-is-custom ? Theme.accent : Theme.text-secondary; } }
                                HorizontalLayout { spacing: 8px; TextField { text <=> root.microsoft-client-id; placeholder: GuiI18n.text("gui-no-override"); horizontal-stretch: 1; } if root.microsoft-client-id-is-custom : AppButton { text: GuiI18n.text("gui-reset-default"); clicked => { root.reset-microsoft-client-id(); } } }
                                BodyText { text: GuiI18n.value(root.microsoft-client-id-is-custom ? "gui-effective-value" : "gui-effective-default", root.effective-microsoft-client-id); color: Theme.text-muted; wrap: word-wrap; }
                            } }
                            Rectangle { height: 146px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 16px; spacing: 8px;
                                HorizontalLayout { Heading { text: GuiI18n.text("gui-user-agent"); font-size: Theme.row-heading-size; } Rectangle { horizontal-stretch: 1; } Pill { text: root.user-agent-is-custom ? GuiI18n.text("gui-override") : GuiI18n.text("gui-default"); tone-color: root.user-agent-is-custom ? Theme.accent : Theme.text-secondary; } }
                                HorizontalLayout { spacing: 8px; TextField { text <=> root.user-agent; placeholder: GuiI18n.text("gui-no-override"); horizontal-stretch: 1; } if root.user-agent-is-custom : AppButton { text: GuiI18n.text("gui-reset-default"); clicked => { root.reset-user-agent(); } } }
                                BodyText { text: GuiI18n.value(root.user-agent-is-custom ? "gui-effective-value" : "gui-effective-default", root.effective-user-agent); color: Theme.text-muted; wrap: word-wrap; }
                            } }
                            HorizontalLayout { BodyText { text: GuiI18n.text("gui-curseforge-unavailable"); color: Theme.text-muted; wrap: word-wrap; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-api-settings"); tone: "primary"; clicked => { root.save-api-settings(root.microsoft-client-id, root.user-agent); } } }
                        }
                        if root.workspace-tab == "Storage" : VerticalLayout { spacing: 14px;
                            SectionTitle { title: GuiI18n.text("gui-persistent-storage"); detail: GuiI18n.text("gui-persistent-storage-detail"); }
                            Rectangle { height: 164px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 16px; spacing: 9px;
                                HorizontalLayout { Heading { text: GuiI18n.text("gui-storage-root"); font-size: Theme.row-heading-size; } Rectangle { horizontal-stretch: 1; } Pill { text: root.persistent-root-is-custom ? GuiI18n.text("gui-custom-location") : GuiI18n.text("gui-default-location"); tone-color: root.persistent-root-is-custom ? Theme.accent : Theme.text-secondary; } }
                                HorizontalLayout { spacing: 8px; TextField { text <=> root.persistent-root; placeholder: GuiI18n.text("gui-no-override"); horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-choose-folder"); clicked => { root.choose-storage-root(); } } }
                                BodyText { text: GuiI18n.value("gui-effective-location", root.storage-root); color: Theme.text-secondary; overflow: elide; }
                                BodyText { text: GuiI18n.value("gui-default-location-path", root.default-persistent-root); color: Theme.text-muted; overflow: elide; }
                            } }
                            HorizontalLayout { if root.persistent-root-is-custom : AppButton { text: GuiI18n.text("gui-reset-default"); clicked => { root.reset-storage-root(); } } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-storage-root"); tone: "primary"; clicked => { root.save-storage-settings(root.persistent-root); } } }
                            BodyText { text: GuiI18n.text("gui-storage-change-future-only"); color: Theme.text-muted; wrap: word-wrap; }
                        }
                        if root.workspace-tab == "About & Licenses" : VerticalLayout { alignment: start; spacing: 14px;
                            Image { source: root.logo; width: 92px; height: 92px; image-fit: contain; accessible-label: GuiI18n.text("gui-logo-accessible"); }
                            Heading { text: "Ephervex"; font-size: 24px; }
                            BodyText { text: GuiI18n.value("gui-version", root.app-version); }
                            BodyText { text: GuiI18n.text("gui-apache-license"); color: Theme.text; }
                            HorizontalLayout { AppButton { text: GuiI18n.text("gui-view-ephervex-license"); clicked => { root.modal = "license"; } } Rectangle { horizontal-stretch: 1; } }
                            Divider { }
                            Heading { text: GuiI18n.text("gui-built-with-slint"); }
                            Rectangle { width: 290px; height: 105px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; AboutSlint { } }
                            Divider { }
                            Heading { text: GuiI18n.text("gui-third-party-software"); }
                            BodyText { text: GuiI18n.text("gui-third-party-detail"); }
                            HorizontalLayout { AppButton { text: GuiI18n.text("gui-third-party-licenses"); clicked => { root.modal = "third-party"; } } Rectangle { horizontal-stretch: 1; } }
                        }
                    } } }
                }
            }

            Rectangle { height: 34px; background: #11171a; border-width: 1px; border-color: Theme.border; HorizontalLayout { padding-left: 14px; padding-right: 14px; spacing: 22px; BodyText { text: root.active-operations > 0 ? GuiI18n.count("gui-active-operations", root.active-operations) : GuiI18n.text("gui-no-active-tasks"); color: root.active-operations > 0 ? Theme.accent : Theme.success; font-size: 11px; } Rectangle { horizontal-stretch: 1; } BodyText { text: root.status-message; font-size: 11px; } BodyText { text: root.storage-root; color: Theme.text-muted; font-size: 11px; overflow: elide; width: 300px; } }
            }
        }

        Timer { interval: 4500ms; running: root.toast-one-text != "" && !root.toast-one-error; triggered => { root.toast-one-text = ""; root.toast-slot-available(); } }
        Timer { interval: 9000ms; running: root.toast-one-text != "" && root.toast-one-error; triggered => { root.toast-one-text = ""; root.toast-slot-available(); } }
        Timer { interval: 4500ms; running: root.toast-two-text != "" && !root.toast-two-error; triggered => { root.toast-two-text = ""; root.toast-slot-available(); } }
        Timer { interval: 9000ms; running: root.toast-two-text != "" && root.toast-two-error; triggered => { root.toast-two-text = ""; root.toast-slot-available(); } }
        Timer { interval: 4500ms; running: root.toast-three-text != "" && !root.toast-three-error; triggered => { root.toast-three-text = ""; root.toast-slot-available(); } }
        Timer { interval: 9000ms; running: root.toast-three-text != "" && root.toast-three-error; triggered => { root.toast-three-text = ""; root.toast-slot-available(); } }
        Rectangle {
            x: parent.width - self.width - 22px;
            y: 78px;
            width: min(440px, parent.width - 44px);
            height: parent.height - 126px;
            VerticalLayout { alignment: end; spacing: 8px;
                if root.toast-one-text != "" : ToastPanel { message: root.toast-one-text; error: root.toast-one-error; dismiss => { root.toast-one-text = ""; root.toast-slot-available(); } }
                if root.toast-two-text != "" : ToastPanel { message: root.toast-two-text; error: root.toast-two-error; dismiss => { root.toast-two-text = ""; root.toast-slot-available(); } }
                if root.toast-three-text != "" : ToastPanel { message: root.toast-three-text; error: root.toast-three-error; dismiss => { root.toast-three-text = ""; root.toast-slot-available(); } }
            }
        }

        // Modal overlay and focused workflows.
        if root.modal != "" : Rectangle {
            background: #00000099;
            TouchArea { clicked => { } }
            Rectangle {
                width: root.modal == "license" ? min(820px, parent.width - 80px) : min(620px, parent.width - 80px);
                height: root.modal == "license" ? min(720px, parent.height - 80px) : root.modal == "third-party" ? 340px : root.modal == "mod-plan" ? 430px : root.modal == "eula" ? 440px : root.modal == "kill" ? 300px : root.modal == "delete-instance" || root.modal == "delete-server" ? 330px : 570px;
                x: (parent.width - self.width) / 2;
                y: (parent.height - self.height) / 2;
                border-radius: 8px;
                border-width: 1px;
                border-color: Theme.border;
                background: Theme.elevated;
                VerticalLayout { padding: 22px; spacing: 13px;
                    HorizontalLayout { alignment: center; Heading { text: root.modal == "create-instance" ? GuiI18n.text("gui-modal-new-instance") : root.modal == "import" ? GuiI18n.text("gui-modal-import") : root.modal == "create-server" ? GuiI18n.text("gui-modal-new-server") : root.modal == "mod-plan" ? GuiI18n.text("gui-modal-install-mod") : root.modal == "eula" ? GuiI18n.text("gui-server-eula") : root.modal == "kill" ? GuiI18n.text("gui-modal-force-kill") : root.modal == "delete-instance" ? GuiI18n.text("gui-delete-instance") : root.modal == "delete-server" ? GuiI18n.text("gui-delete-server") : root.modal == "license" ? GuiI18n.text("gui-modal-apache-license") : GuiI18n.text("gui-third-party-licenses"); font-size: 20px; } Rectangle { horizontal-stretch: 1; } IconButton { icon: @image-url("../../../assets/icons/close.svg"); accessible-name: GuiI18n.text("gui-close"); clicked => { root.modal = ""; root.modal-error = ""; } } }
                    Divider { }
                    if root.modal == "create-instance" : VerticalLayout { spacing: 11px; BodyText { text: GuiI18n.text("gui-create-instance-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-name"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-name; placeholder: GuiI18n.text("gui-instance-name-placeholder"); } Text { text: GuiI18n.text("gui-minecraft-release"); color: Theme.text-secondary; font-size: 12px; } ComboBox { model: root.minecraft-releases; current-value <=> root.create-minecraft; selected(value) => { root.create-minecraft = value; root.create-loader-version = ""; root.create-loader-versions = []; if root.create-loader != "Vanilla" { root.request-loader-catalog("client", root.create-loader, value); } } } Text { text: GuiI18n.text("gui-loader"); color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["Vanilla", "Forge", "NeoForge"]; current-index: root.create-loader == "Forge" ? 1 : root.create-loader == "NeoForge" ? 2 : 0; selected(value) => { root.create-loader = value; root.create-loader-version = ""; root.create-loader-versions = []; if value != "Vanilla" { root.request-loader-catalog("client", value, root.create-minecraft); } } } if root.create-loader != "Vanilla" : ComboBox { model: root.create-loader-versions; current-value <=> root.create-loader-version; } HorizontalLayout { BodyText { text: GuiI18n.text("gui-store-persistently"); } Toggle { checked <=> root.create-persistent; } BodyText { text: root.create-persistent ? GuiI18n.text("gui-persistent-root") : GuiI18n.text("gui-temporary-root"); color: Theme.text-muted; } } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-create-instance"); tone: "primary"; enabled: root.minecraft-releases.length > 0 && root.create-name != "" && root.create-minecraft != "" && (root.create-loader == "Vanilla" || root.create-loader-version != ""); clicked => { root.submit-create-instance(root.create-name, root.create-minecraft, root.create-loader, root.create-loader-version, root.create-persistent); } } } }
                    if root.modal == "import" : VerticalLayout { spacing: 11px; BodyText { text: GuiI18n.text("gui-import-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-modpack-archive"); color: Theme.text-secondary; font-size: 12px; } HorizontalLayout { TextField { text <=> root.import-path; placeholder: GuiI18n.text("gui-modpack-placeholder"); horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-browse"); clicked => { root.import-analysis = ""; root.choose-import-file(); } } } if root.import-analysis != "" : Rectangle { height: 64px; border-radius: 6px; border-width: 1px; border-color: Theme.success; background: Theme.surface; BodyText { x: 12px; width: parent.width - 24px; text: GuiI18n.value("gui-validated", root.import-analysis); wrap: word-wrap; } } Text { text: GuiI18n.text("gui-instance-name-optional"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.import-name; placeholder: GuiI18n.text("gui-detected-pack-name"); } HorizontalLayout { BodyText { text: GuiI18n.text("gui-store-persistently"); } Toggle { checked <=> root.create-persistent; } } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-import-modpack"); tone: "primary"; enabled: root.import-path != ""; clicked => { root.submit-import(root.import-path, root.import-name, root.create-persistent); } } } }
                    if root.modal == "create-server" : ScrollView { VerticalLayout { spacing: 10px; BodyText { text: root.create-server-client-id != "" ? GuiI18n.text("gui-create-linked-server-detail") : GuiI18n.text("gui-create-standalone-server-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-name"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-server-name; placeholder: GuiI18n.text("gui-server-name-placeholder"); } if root.create-server-client-id == "" : Text { text: GuiI18n.text("gui-minecraft-release"); color: Theme.text-secondary; font-size: 12px; } if root.create-server-client-id == "" : ComboBox { model: root.minecraft-releases; current-value <=> root.create-server-minecraft; selected(value) => { root.create-server-minecraft = value; root.create-server-loader-version = ""; root.create-server-loader-versions = []; if root.create-server-loader != "Vanilla" { root.request-loader-catalog("server", root.create-server-loader, value); } } } if root.create-server-client-id == "" : Text { text: GuiI18n.text("gui-loader"); color: Theme.text-secondary; font-size: 12px; } if root.create-server-client-id == "" : ComboBox { model: ["Vanilla", "Forge", "NeoForge"]; current-index: root.create-server-loader == "Forge" ? 1 : root.create-server-loader == "NeoForge" ? 2 : 0; selected(value) => { root.create-server-loader = value; root.create-server-loader-version = ""; root.create-server-loader-versions = []; if value != "Vanilla" { root.request-loader-catalog("server", value, root.create-server-minecraft); } } } if root.create-server-client-id == "" && root.create-server-loader != "Vanilla" : ComboBox { model: root.create-server-loader-versions; current-value <=> root.create-server-loader-version; } HorizontalLayout { BodyText { text: GuiI18n.text("gui-store-persistently"); } Toggle { checked <=> root.create-server-persistent; } } BodyText { text: GuiI18n.text("gui-eula-create-warning"); color: Theme.warning; wrap: word-wrap; } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-create-server"); tone: "primary"; enabled: root.create-server-name != "" && (root.create-server-client-id != "" || (root.minecraft-releases.length > 0 && root.create-server-minecraft != "" && (root.create-server-loader == "Vanilla" || root.create-server-loader-version != ""))); clicked => { root.submit-create-server(root.create-server-name, root.create-server-client-id, root.create-server-minecraft, root.create-server-loader, root.create-server-loader-version, root.create-server-persistent); } } } } }
                    if root.modal == "mod-plan" : VerticalLayout { spacing: 12px; Heading { text: root.mod-plan-title + " " + root.mod-plan-version; font-size: 16px; } BodyText { text: GuiI18n.text("gui-mod-plan-compatible"); wrap: word-wrap; } Rectangle { height: 116px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; VerticalLayout { padding: 12px; spacing: 5px; BodyText { text: GuiI18n.count("gui-mod-plan-install", root.mod-plan-install-count); color: Theme.text; } BodyText { text: GuiI18n.count("gui-mod-plan-required", root.mod-plan-dependency-count); } BodyText { text: GuiI18n.count("gui-mod-plan-existing", root.mod-plan-existing-count); } BodyText { text: GuiI18n.count("gui-mod-plan-optional", root.mod-plan-optional-count); } } } if root.mod-plan-warning-count > 0 : BodyText { text: GuiI18n.count("gui-mod-plan-warnings", root.mod-plan-warning-count); color: Theme.warning; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-install-required"); tone: "primary"; enabled: root.mod-plan-install-count > 0; clicked => { root.install-mod(root.mod-plan-instance, root.mod-plan-project); root.modal = ""; } } } }
                    if root.modal == "eula" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-eula-detail"); wrap: word-wrap; } AppButton { text: GuiI18n.text("gui-open-minecraft-eula"); clicked => { root.open-eula-reference(); } } Rectangle { height: 70px; border-radius: 6px; border-width: 1px; border-color: root.eula-accepted ? Theme.success : Theme.warning; background: Theme.surface; BodyText { x: 12px; width: parent.width - 24px; text: root.eula-status; wrap: word-wrap; } } if !root.eula-accepted : BodyText { text: GuiI18n.text("gui-eula-confirm"); color: Theme.text; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } if root.eula-accepted : AppButton { text: GuiI18n.text("gui-revoke-consent"); tone: "danger"; clicked => { root.revoke-eula(); } } if !root.eula-accepted : AppButton { text: GuiI18n.text("gui-accept-eula"); tone: "primary"; clicked => { root.accept-eula(); } } } }
                    if root.modal == "kill" : VerticalLayout { spacing: 14px; BodyText { text: GuiI18n.value("gui-force-kill-detail", root.pending-action-name != "" ? root.pending-action-name : root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : GuiI18n.text("gui-this-server")); color: Theme.warning; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-force-kill-action"); tone: "danger"; clicked => { root.server-kill(root.selected-server-id); root.modal = ""; } } } }
                    if root.modal == "delete-instance" || root.modal == "delete-server" : VerticalLayout { spacing: 14px; BodyText { text: GuiI18n.value(root.modal == "delete-instance" ? "gui-delete-instance-detail" : "gui-delete-server-detail", root.pending-action-name); color: Theme.warning; wrap: word-wrap; } BodyText { text: GuiI18n.text("gui-delete-choice-detail"); wrap: word-wrap; } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-remove-from-ephervex"); clicked => { if root.modal == "delete-instance" { root.delete-client(root.pending-action-id, false); } else { root.delete-server(root.pending-action-id, false); } } } AppButton { text: GuiI18n.text("gui-delete-files"); tone: "danger"; clicked => { if root.modal == "delete-instance" { root.delete-client(root.pending-action-id, true); } else { root.delete-server(root.pending-action-id, true); } } } } }
                    if root.modal == "license" : ScrollView { Text { text: root.license-text; color: Theme.text-secondary; font-family: "monospace"; font-size: 12px; wrap: word-wrap; } }
                    if root.modal == "third-party" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-third-party-unavailable"); wrap: word-wrap; } BodyText { text: GuiI18n.text("gui-slint-attribution"); color: Theme.text-muted; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-close"); tone: "primary"; clicked => { root.modal = ""; } } } }
                }
            }
        }
    }
}
