slint::slint! {
    import { AboutSlint, ComboBox, ScrollView, Slider, Spinner } from "std-widgets.slint";

    export global GuiI18n {
        pure callback text(string) -> string;
        pure callback count(string, int) -> string;
        pure callback value(string, string) -> string;
        pure callback values(string, string, string) -> string;
    }

    export global Theme {
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
        enabled: bool,
        kind: string,
        syntax: string,
        numeric-value: float,
    }

    component BodyText inherits Text {
        color: Theme.text-secondary;
        font-size: 13px;
        vertical-alignment: center;
        vertical-stretch: 0;
    }

    component Heading inherits Text {
        color: Theme.text;
        font-size: 17px;
        font-weight: 600;
        vertical-alignment: center;
        horizontal-stretch: 0;
        vertical-stretch: 0;
    }

    component Divider inherits Rectangle {
        height: 1px;
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
        background: !enabled ? Theme.surface : area.pressed ? Theme.background : area.has-hover ? Theme.elevated : tone == "primary" ? #2d7d84 : Theme.surface;
        accessible-role: button;
        accessible-label: text;
        accessible-enabled: enabled;
        label := Text {
            text: root.text;
            color: !root.enabled ? Theme.text-muted : root.tone == "danger" ? Theme.danger : Theme.text;
            font-size: 13px;
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
        in property <string> text;
        in property <bool> enabled: true;
        callback clicked();
        width: 36px;
        height: 36px;
        border-radius: 6px;
        border-width: 1px;
        border-color: area.has-hover ? Theme.accent : Theme.border;
        background: area.has-hover ? Theme.elevated : transparent;
        Text {
            text: root.text;
            color: root.enabled ? Theme.text-secondary : Theme.text-muted;
            font-size: 17px;
            horizontal-alignment: center;
            vertical-alignment: center;
        }
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
            font-size: 13px;
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
            font-size: 13px;
            vertical-alignment: center;
        }
    }

    component FieldLabel inherits Text {
        color: Theme.text-secondary;
        font-size: 12px;
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
            font-size: 11px;
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
        Rectangle { x: 0px; y: 8px; width: 3px; height: 30px; background: root.selected ? Theme.accent : transparent; border-radius: 2px; }
        HorizontalLayout {
            padding-left: 16px; padding-right: 12px; spacing: 13px;
            Image { source: root.icon; width: 22px; height: 22px; image-fit: contain; }
            Text { text: root.label; color: root.selected ? Theme.accent : Theme.text-secondary; font-size: 14px; vertical-alignment: center; }
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

    component InstanceCard inherits Rectangle {
        in property <InstanceRow> item;
        in property <bool> selected;
        callback choose(string);
        callback launch(string);
        callback open-workspace(string);
        callback open-folder(string);
        callback create-server(string);
        callback duplicate(string);
        callback delete(string, string);
        height: 112px;
        border-radius: 8px;
        border-width: selected ? 2px : 1px;
        border-color: selected ? Theme.accent : Theme.border;
        background: selected ? #172329 : Theme.surface;
        HorizontalLayout {
            padding: 15px; spacing: 16px;
            LoaderGlyph { kind: item.loader-kind; }
            VerticalLayout {
                spacing: 5px;
                HorizontalLayout {
                    spacing: 9px;
                    Heading { text: item.name; }
                    Pill { text: item.storage; tone-color: item.storage-kind == "temporary" ? Theme.warning : Theme.success; }
                    if item.needs-attention : Pill { text: GuiI18n.text("gui-attention"); tone-color: Theme.danger; }
                }
                BodyText { text: GuiI18n.values("gui-target-summary", item.minecraft, item.loader) + (item.mods > 0 ? " | " + GuiI18n.count("gui-managed-mods-count", item.mods) : ""); }
                BodyText { text: item.linked-servers > 0 ? GuiI18n.count("gui-linked-servers-count", item.linked-servers) : GuiI18n.value("gui-source-instance", item.source); color: item.linked-servers > 0 ? Theme.accent : Theme.text-muted; }
            }
            VerticalLayout {
                width: 150px; alignment: center; spacing: 7px;
                BodyText { text: item.updated; horizontal-alignment: center; }
                AppButton { text: item.can-launch ? GuiI18n.text("gui-launch") : GuiI18n.text("gui-launch-unavailable"); tone: item.can-launch ? "primary" : "secondary"; enabled: item.can-launch; clicked => { root.launch(item.id); } }
            }
        }
        TouchArea { width: parent.width - 176px; clicked => { root.choose(item.id); } double-clicked => { root.open-workspace(item.id); } }
        ContextMenuArea {
            Menu {
                MenuItem { title: GuiI18n.text("gui-open-workspace"); activated => { root.open-workspace(item.id); } }
                MenuItem { title: GuiI18n.text("gui-launch"); enabled: item.can-launch; activated => { root.launch(item.id); } }
                MenuItem { title: GuiI18n.text("gui-open-folder"); activated => { root.open-folder(item.id); } }
                MenuItem { title: GuiI18n.text("gui-create-linked-server"); activated => { root.create-server(item.id); } }
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
        background: selected ? #172329 : Theme.surface;
        HorizontalLayout {
            padding: 15px; spacing: 16px;
            LoaderGlyph { kind: item.loader-kind; server: true; }
            VerticalLayout {
                spacing: 5px;
                HorizontalLayout {
                    spacing: 9px;
                    Heading { text: item.name; }
                    Pill { text: item.storage; tone-color: item.storage-kind == "temporary" ? Theme.warning : Theme.success; }
                    Pill { text: item.state; tone-color: item.state-kind == "running" ? Theme.success : item.state-kind == "stopped" ? Theme.text-secondary : Theme.warning; }
                }
                BodyText { text: GuiI18n.values("gui-target-summary", item.minecraft, item.loader) + " | " + item.relationship; }
                BodyText { text: item.address + " | " + item.state-detail; color: Theme.text-muted; }
            }
            VerticalLayout {
                width: 128px; alignment: center;
                AppButton {
                    text: item.can-start ? GuiI18n.text("gui-start") : item.can-stop ? GuiI18n.text("gui-stop") : GuiI18n.text("gui-unavailable");
                    tone: item.can-start ? "primary" : "secondary";
                    enabled: item.can-start || item.can-stop;
                    clicked => { root.primary(item.id); }
                }
            }
        }
        TouchArea { width: parent.width - 154px; clicked => { root.choose(item.id); root.open-workspace(item.id); } }
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
        callback create-server(string);
        callback diagnostics(string);
        background: Theme.surface;
        border-width: 1px;
        border-color: Theme.border;
        if !has-selection : EmptyState { title: GuiI18n.text("gui-no-instance-selected"); message: GuiI18n.text("gui-select-instance"); }
        if has-selection : VerticalLayout {
            padding: 22px; spacing: 14px;
            Heading { text: item.name; font-size: 18px; }
            Divider { }
            BodyText { text: GuiI18n.text("gui-overview"); color: Theme.text-muted; font-size: 11px; }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-status"); } BodyText { text: item.needs-attention ? GuiI18n.text("gui-needs-attention") : item.can-launch ? GuiI18n.text("gui-ready-to-launch") : GuiI18n.text("gui-busy"); color: item.needs-attention ? Theme.warning : Theme.success; } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-minecraft"); } BodyText { text: item.minecraft; } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-loader"); } BodyText { text: item.loader; } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-source"); } BodyText { text: item.source; } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-mods"); } BodyText { text: GuiI18n.count("gui-managed-count", item.mods); } }
            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-storage"); } BodyText { text: item.storage; } }
            BodyText { text: item.path; color: Theme.text-muted; overflow: elide; }
            Divider { }
            BodyText { text: GuiI18n.text("gui-actions"); color: Theme.text-muted; font-size: 11px; }
            AppButton { text: GuiI18n.text("gui-launch"); tone: "primary"; enabled: item.can-launch; clicked => { root.launch(item.id); } }
            if item.connect-server-id != "" : AppButton { text: GuiI18n.value("gui-launch-connect", item.connect-server-name); enabled: item.can-connect; clicked => { root.connect(item.id, item.connect-server-id); } }
            AppButton { text: GuiI18n.text("gui-open-workspace"); clicked => { root.workspace(item.id); } }
            AppButton { text: GuiI18n.text("gui-create-linked-server"); clicked => { root.create-server(item.id); } }
            AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-folder(item.id); } }
            Divider { }
            BodyText { text: GuiI18n.text("gui-diagnostics"); color: Theme.text-muted; font-size: 11px; }
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
        in-out property <string> toast-text;
        in-out property <bool> toast-error;
        in-out property <string> pending-action-id;
        in-out property <string> pending-action-name;
        in-out property <bool> show-inspector: true;
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
        in-out property <string> user-agent;
        in-out property <string> persistent-root;
        in-out property <string> java-memory-min: "512";
        in-out property <string> java-memory-max: "4096";
        in-out property <string> java-preferred-path;
        in-out property <bool> java-auto-detect: true;
        in-out property <bool> java-auto-download: true;

        callback refresh();
        callback request-version-catalog();
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
        callback save-storage-settings(string);
        callback refresh-java();
        callback save-java-settings(bool, bool, string, string, string);
        callback save-preference(string, string, bool);
        callback cancel-operation(string);

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
                    padding-left: 20px; padding-right: 20px; spacing: 14px;
                    Image { source: root.logo; width: 36px; height: 36px; image-fit: contain; }
                    Heading { text: "Ephervex"; font-size: 20px; width: 180px; }
                    Rectangle { horizontal-stretch: 1; }
                    if root.page == 0 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-import-modpack"); clicked => { root.modal = "import"; } }
                    if root.page == 0 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-new-instance"); tone: "primary"; min-width: 148px; clicked => { root.modal-error = ""; root.request-version-catalog(); root.modal = "create-instance"; } }
                    if root.page == 1 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-new-server"); tone: "primary"; min-width: 130px; clicked => { root.create-server-client-id = ""; root.modal-error = ""; root.request-version-catalog(); root.modal = "create-server"; } }
                    AppButton { text: GuiI18n.text("gui-refresh"); clicked => { root.refresh(); } }
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
                        NavItem { icon: @image-url("../../../assets/icons/nav-logs.svg"); label: GuiI18n.text("gui-logs"); selected: root.page == 2; clicked => { root.page = 2; root.workspace = ""; root.load-diagnostics("launcher", "", "log"); } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-activity.svg"); label: GuiI18n.text("gui-activity"); selected: root.page == 3; clicked => { root.page = 3; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-settings.svg"); label: GuiI18n.text("gui-settings"); selected: root.page == 4; clicked => { root.page = 4; root.workspace = ""; root.workspace-tab = "General"; } }
                        Rectangle { vertical-stretch: 1; }
                        BodyText { text: GuiI18n.text("gui-tagline"); color: Theme.text-muted; font-size: 11px; wrap: word-wrap; }
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
                            padding: 22px; spacing: 16px;
                            HorizontalLayout { Heading { text: GuiI18n.text("gui-instances"); font-size: 25px; } Rectangle { horizontal-stretch: 1; } BodyText { text: GuiI18n.count("gui-client-instances-count", root.instances.length); } }
                            HorizontalLayout {
                                spacing: 8px;
                                AppButton { text: GuiI18n.text("gui-all"); tone: root.instance-filter == "all" ? "primary" : "secondary"; min-width: 70px; clicked => { root.instance-filter = "all"; root.refresh(); } }
                                AppButton { text: GuiI18n.text("gui-temporary"); tone: root.instance-filter == "temporary" ? "primary" : "secondary"; clicked => { root.instance-filter = "temporary"; root.refresh(); } }
                                AppButton { text: GuiI18n.text("gui-persistent"); tone: root.instance-filter == "persistent" ? "primary" : "secondary"; clicked => { root.instance-filter = "persistent"; root.refresh(); } }
                                Rectangle { horizontal-stretch: 1; }
                                TextField { text <=> root.instance-query; placeholder: GuiI18n.text("gui-filter-instances"); input-type: InputType.search; width: 290px; accepted => { root.refresh(); } }
                            }
                            if root.loading : VerticalLayout { alignment: center; Spinner { width: 38px; height: 38px; } BodyText { text: GuiI18n.text("gui-loading-instances"); horizontal-alignment: center; } }
                            if !root.loading && root.instances.length == 0 : EmptyState {
                                title: GuiI18n.text("gui-no-client-instances");
                                message: GuiI18n.text("gui-no-client-instances-detail");
                                primary-text: GuiI18n.text("gui-modal-new-instance"); secondary-text: GuiI18n.text("gui-import-modpack");
                                primary => { root.modal-error = ""; root.request-version-catalog(); root.modal = "create-instance"; }
                                secondary => { root.modal = "import"; }
                            }
                            if !root.loading && root.instances.length > 0 : ScrollView {
                                viewport-width: self.width;
                                VerticalLayout {
                                    spacing: 12px;
                                    for item[index] in root.instances : InstanceCard {
                                        visible: root.instance-filter == "all" || item.storage-kind == root.instance-filter;
                                        item: item;
                                        selected: item.id == root.selected-instance-id;
                                        choose(id) => { root.selected-instance-id = id; root.select-instance(id); }
                                        launch(id) => { root.launch-client(id); }
                                        open-workspace(id) => { root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); }
                                        open-folder(id) => { root.open-client-folder(id); }
                                        create-server(id) => { root.create-server-client-id = id; root.modal = "create-server"; }
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
                        create-server(id) => { root.create-server-client-id = id; root.modal = "create-server"; }
                        diagnostics(id) => { root.page = 2; root.load-diagnostics("client", id, "log"); }
                    }
                }

                if root.page == 0 && root.workspace == "instance" : Rectangle {
                    horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout {
                        padding: 22px; spacing: 14px;
                        HorizontalLayout { AppButton { text: GuiI18n.text("gui-back-instances"); clicked => { root.workspace = ""; } } Heading { text: root.selected-instance-index >= 0 ? root.instances[root.selected-instance-index].name : GuiI18n.text("gui-instance"); font-size: 23px; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-launch"); tone: "primary"; enabled: root.selected-instance-index >= 0 && root.instances[root.selected-instance-index].can-launch; clicked => { root.launch-client(root.selected-instance-id); } } }
                        HorizontalLayout {
                            spacing: 8px;
                            AppButton { text: GuiI18n.text("gui-instance-tab-overview"); tone: root.workspace-tab == "Overview" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Overview"; } }
                            AppButton { text: GuiI18n.text("gui-instance-tab-mods"); tone: root.workspace-tab == "Mods" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Mods"; } }
                            AppButton { text: GuiI18n.text("gui-instance-tab-minecraft-settings"); tone: root.workspace-tab == "Minecraft Settings" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Minecraft Settings"; } }
                            AppButton { text: GuiI18n.text("gui-instance-tab-logs"); clicked => { root.page = 2; root.load-diagnostics("client", root.selected-instance-id, "log"); } }
                        }
                        Divider { }
                        if root.workspace-tab == "Overview" : ScrollView {
                            VerticalLayout { spacing: 12px;
                                Heading { text: GuiI18n.text("gui-reproduction-environment"); }
                                BodyText { text: GuiI18n.text("gui-reproduction-environment-detail"); }
                                Rectangle { height: 126px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    HorizontalLayout { padding: 18px; spacing: 18px; LoaderGlyph { kind: root.instances[root.selected-instance-index].loader-kind; } VerticalLayout { Heading { text: root.instances[root.selected-instance-index].name; } BodyText { text: GuiI18n.values("gui-target-summary", root.instances[root.selected-instance-index].minecraft, root.instances[root.selected-instance-index].loader); } BodyText { text: root.instances[root.selected-instance-index].storage + " | " + root.instances[root.selected-instance-index].source; } BodyText { text: root.instances[root.selected-instance-index].path; color: Theme.text-muted; } } }
                                }
                                HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-client-folder(root.selected-instance-id); } } AppButton { text: GuiI18n.text("gui-create-linked-server"); clicked => { root.create-server-client-id = root.selected-instance-id; root.modal = "create-server"; } } AppButton { text: GuiI18n.text("gui-open-diagnostics"); clicked => { root.page = 2; root.load-diagnostics("client", root.selected-instance-id, "log"); } } }
                                BodyText { text: root.instances[root.selected-instance-index].storage-kind == "temporary" ? GuiI18n.text("gui-temporary-lifecycle") : GuiI18n.text("gui-persistent-lifecycle"); color: Theme.text-muted; wrap: word-wrap; }
                            }
                        }
                        if root.workspace-tab == "Mods" : HorizontalLayout {
                            spacing: 16px;
                            Rectangle { width: 42%; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                VerticalLayout { padding: 16px; spacing: 10px; Heading { text: GuiI18n.text("gui-managed-mods"); } BodyText { text: GuiI18n.text("gui-managed-mods-detail"); wrap: word-wrap; }
                                    if root.managed-mods.length == 0 : EmptyState { title: GuiI18n.text("gui-no-managed-mods"); message: GuiI18n.text("gui-no-managed-mods-detail"); }
                                    ScrollView { VerticalLayout { spacing: 8px; for mod in root.managed-mods : Rectangle { height: 70px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; VerticalLayout { padding: 10px; Heading { text: mod.title; font-size: 14px; } BodyText { text: mod.version + " | " + mod.provider + " | " + mod.origin; } } } } }
                                }
                            }
                            Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                VerticalLayout { padding: 16px; spacing: 10px; Heading { text: GuiI18n.text("gui-add-modrinth-mods"); } BodyText { text: GuiI18n.text("gui-modrinth-compatibility"); wrap: word-wrap; }
                                    HorizontalLayout { TextField { text <=> root.mod-query; placeholder: GuiI18n.text("gui-search-compatible-mods"); input-type: InputType.search; horizontal-stretch: 1; accepted => { root.search-mods(root.selected-instance-id, root.mod-query); } } AppButton { text: GuiI18n.text("gui-search"); tone: "primary"; clicked => { root.search-mods(root.selected-instance-id, root.mod-query); } } }
                                    ScrollView { VerticalLayout { spacing: 8px; for mod in root.mod-results : Rectangle { height: 92px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; HorizontalLayout { padding: 10px; VerticalLayout { Heading { text: mod.title; font-size: 14px; } BodyText { text: mod.description; wrap: word-wrap; } BodyText { text: GuiI18n.values("gui-mod-result-meta", mod.author, mod.downloads); color: Theme.text-muted; } } AppButton { text: GuiI18n.text("gui-review"); clicked => { root.review-mod(root.selected-instance-id, mod.project-id); } } } } } }
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
                        VerticalLayout { padding: 22px; spacing: 16px;
                            HorizontalLayout { Heading { text: GuiI18n.text("gui-servers"); font-size: 25px; } Rectangle { horizontal-stretch: 1; } TextField { text <=> root.server-query; placeholder: GuiI18n.text("gui-filter-servers"); input-type: InputType.search; width: 290px; accepted => { root.refresh(); } } }
                            if root.servers.length == 0 : EmptyState { title: GuiI18n.text("gui-no-servers"); message: GuiI18n.text("gui-no-servers-detail"); primary-text: GuiI18n.text("gui-modal-new-server"); primary => { root.create-server-client-id = ""; root.modal-error = ""; root.request-version-catalog(); root.modal = "create-server"; } }
                            if root.servers.length > 0 : ScrollView { viewport-width: self.width; VerticalLayout { spacing: 12px; for item in root.servers : ServerCard { item: item; selected: item.id == root.selected-server-id; choose(id) => { root.selected-server-id = id; root.select-server(id); } primary(id) => { if item.can-start { root.server-start(id); } else { root.server-stop(id); } } open-workspace(id) => { root.selected-server-id = id; root.select-server(id); root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(id); } open-folder(id) => { root.open-server-folder(id); } restart(id) => { root.server-restart(id); } kill(id, name) => { root.selected-server-id = id; root.pending-action-name = name; root.modal = "kill"; } delete(id, name) => { root.pending-action-id = id; root.pending-action-name = name; root.modal = "delete-server"; } } } }
                        }
                    }
                    if root.show-inspector && root.selected-server-index >= 0 : Rectangle { width: 360px; background: Theme.surface; border-width: 1px; border-color: Theme.border;
                        if root.selected-server-index < 0 : EmptyState { title: GuiI18n.text("gui-no-server-selected"); message: GuiI18n.text("gui-select-server"); }
                        if root.selected-server-index >= 0 : VerticalLayout { padding: 22px; spacing: 12px;
                            HorizontalLayout { Heading { text: root.servers[root.selected-server-index].name; } Rectangle { horizontal-stretch: 1; } Pill { text: root.servers[root.selected-server-index].state; tone-color: root.servers[root.selected-server-index].state-kind == "running" ? Theme.success : Theme.text-secondary; } }
                            Divider { }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-target"); } BodyText { text: root.servers[root.selected-server-index].minecraft + " | " + root.servers[root.selected-server-index].loader; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-relation"); } BodyText { text: root.servers[root.selected-server-index].relationship; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-address"); } BodyText { text: root.servers[root.selected-server-index].address; } }
                            Divider { }
                            AppButton { text: GuiI18n.text("gui-open-server-workspace"); tone: "primary"; clicked => { root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(root.selected-server-id); } }
                            HorizontalLayout { spacing: 8px; AppButton { text: root.servers[root.selected-server-index].can-start ? GuiI18n.text("gui-start") : GuiI18n.text("gui-stop"); enabled: root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop; clicked => { if root.servers[root.selected-server-index].can-start { root.server-start(root.selected-server-id); } else { root.server-stop(root.selected-server-id); } } } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } }
                            if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: GuiI18n.text("gui-launch-client-connect"); clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } }
                            AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(root.selected-server-id); } }
                            Divider { }
                            AppButton { text: GuiI18n.text("gui-force-kill"); tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } }
                        }
                    }
                }

                if root.page == 1 && root.workspace == "server" : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 22px; spacing: 14px;
                        HorizontalLayout { AppButton { text: GuiI18n.text("gui-back-servers"); clicked => { root.workspace = ""; } } Heading { text: root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : GuiI18n.text("gui-server"); font-size: 23px; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-stop ? GuiI18n.text("gui-stop") : GuiI18n.text("gui-start"); tone: "primary"; enabled: root.selected-server-index >= 0 && (root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop); clicked => { if root.servers[root.selected-server-index].can-stop { root.server-stop(root.selected-server-id); } else { root.server-start(root.selected-server-id); } } } }
                        HorizontalLayout { spacing: 8px;
                            AppButton { text: GuiI18n.text("gui-instance-tab-overview"); tone: root.workspace-tab == "Overview" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Overview"; } }
                            AppButton { text: GuiI18n.text("gui-server-tab-console"); tone: root.workspace-tab == "Console" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Console"; } }
                            AppButton { text: GuiI18n.text("gui-server-tab-properties"); tone: root.workspace-tab == "Properties" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Properties"; } }
                            AppButton { text: GuiI18n.text("gui-instance-tab-logs"); clicked => { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } }
                        }
                        Divider { }
                        if root.workspace-tab == "Overview" : ScrollView { VerticalLayout { alignment: start; spacing: 12px; Heading { text: GuiI18n.text("gui-dedicated-server"); } BodyText { text: root.servers[root.selected-server-index].state-detail; } Rectangle { height: 160px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 18px; spacing: 7px;
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-exact-target"); } BodyText { text: GuiI18n.values("gui-target-summary", root.servers[root.selected-server-index].minecraft, root.servers[root.selected-server-index].loader); } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-runtime"); } BodyText { text: root.servers[root.selected-server-index].state + " | " + root.servers[root.selected-server-index].state-detail; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-network"); } BodyText { text: root.servers[root.selected-server-index].address; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-storage"); } BodyText { text: root.servers[root.selected-server-index].storage; } }
                            HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-relationship"); } BodyText { text: root.servers[root.selected-server-index].relationship; } }
                        } } HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-view-logs"); clicked => { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } } if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: GuiI18n.text("gui-launch-client-connect"); tone: "primary"; clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } } } } }
                        if root.workspace-tab == "Console" : VerticalLayout { spacing: 10px;
                            HorizontalLayout { BodyText { text: GuiI18n.value("gui-bounded-output", root.servers[root.selected-server-index].state); color: root.servers[root.selected-server-index].can-send-command ? Theme.success : Theme.text-secondary; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-stop"); enabled: root.servers[root.selected-server-index].can-stop; clicked => { root.server-stop(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-kill"); tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } } }
                            SelectableText { vertical-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; text: root.console-text; }
                            HorizontalLayout { TextField { text <=> root.console-command; placeholder: root.servers[root.selected-server-index].can-send-command ? GuiI18n.text("gui-enter-server-command") : GuiI18n.text("gui-start-server-command"); enabled: root.servers[root.selected-server-index].can-send-command; horizontal-stretch: 1; accepted => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } AppButton { text: GuiI18n.text("gui-send"); tone: "primary"; enabled: root.servers[root.selected-server-index].can-send-command && root.console-command != ""; clicked => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } }
                        }
                        if root.workspace-tab == "Properties" : ScrollView { VerticalLayout { spacing: 14px;
                            Heading { text: GuiI18n.text("gui-network"); } BodyText { text: GuiI18n.text("gui-network-detail"); wrap: word-wrap; }
                            HorizontalLayout { spacing: 12px; VerticalLayout { FieldLabel { text: GuiI18n.text("gui-access"); } ComboBox { model: [GuiI18n.text("gui-access-localhost"), GuiI18n.text("gui-access-lan"), GuiI18n.text("gui-access-custom")]; current-index: root.property-access == "LAN" ? 1 : root.property-access == "Custom" ? 2 : 0; selected(value) => { root.property-access = value == GuiI18n.text("gui-access-lan") ? "LAN" : value == GuiI18n.text("gui-access-custom") ? "Custom" : "Localhost"; root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } width: 190px; } } VerticalLayout { FieldLabel { text: GuiI18n.text("gui-port"); } TextField { text <=> root.property-port; placeholder: "25565"; input-type: InputType.number; invalid: root.property-port-error != ""; width: 150px; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } if root.property-port-error != "" : Text { text: root.property-port-error; color: Theme.danger; font-size: 11px; } } VerticalLayout { horizontal-stretch: 1; FieldLabel { text: GuiI18n.text("gui-custom-bind-address"); } TextField { text <=> root.property-address; placeholder: "127.0.0.1 or ::1"; enabled: root.property-access == "Custom"; invalid: root.property-address-error != ""; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } if root.property-address-error != "" : Text { text: root.property-address-error; color: Theme.danger; font-size: 11px; } } }
                            HorizontalLayout { spacing: 10px; LabeledToggle { label: GuiI18n.text("gui-online-mode"); checked <=> root.property-online-mode; } if !root.property-online-mode && root.property-access != "Localhost" : BodyText { text: GuiI18n.text("gui-network-warning"); color: Theme.warning; } }
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
                        HorizontalLayout { Heading { text: GuiI18n.text("gui-logs-crash-reports"); font-size: 25px; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-client"); clicked => { if root.selected-instance-id != "" { root.load-diagnostics("client", root.selected-instance-id, "log"); } } } AppButton { text: GuiI18n.text("gui-server-source"); clicked => { if root.selected-server-id != "" { root.load-diagnostics("server", root.selected-server-id, "log"); } } } AppButton { text: GuiI18n.text("gui-launcher"); tone: root.selected-diagnostic-scope == "launcher" ? "primary" : "secondary"; clicked => { root.load-diagnostics("launcher", "", "log"); } } }
                        HorizontalLayout { spacing: 12px;
                            Rectangle { width: 300px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 10px; spacing: 7px;
                                HorizontalLayout { BodyText { text: GuiI18n.count("gui-diagnostic-files-count", root.diagnostics.length); } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-refresh"); clicked => { root.load-diagnostics(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } }
                                if root.diagnostics.length == 0 : EmptyState { title: GuiI18n.text("gui-no-diagnostics"); message: GuiI18n.text("gui-no-diagnostics-detail"); }
                                ScrollView { VerticalLayout { spacing: 5px; for item in root.diagnostics : Rectangle { height: 76px; border-radius: 6px; border-width: item.name == root.selected-diagnostic-name ? 2px : 1px; border-color: item.name == root.selected-diagnostic-name ? Theme.accent : Theme.border; background: Theme.background; VerticalLayout { padding: 8px; Text { text: GuiI18n.value(item.kind == "crash-report" ? "gui-crash-name" : "gui-log-name", item.name); color: item.kind == "crash-report" ? Theme.warning : Theme.text; font-size: 13px; overflow: elide; } BodyText { text: item.entity + " | " + item.modified; font-size: 11px; } BodyText { text: item.summary != "" ? item.summary : item.size; color: Theme.text-muted; font-size: 11px; overflow: elide; } } TouchArea { clicked => { root.selected-diagnostic-name = item.name; root.selected-diagnostic-scope = item.scope-kind; root.selected-diagnostic-id = item.scope-id; root.selected-diagnostic-kind = item.kind; root.open-diagnostic(item.scope-kind, item.scope-id, item.kind, item.name); } } } } }
                            } }
                            Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: rgb(11, 14, 16);
                                VerticalLayout { spacing: 0px;
                                    Rectangle { height: 52px; background: Theme.surface; HorizontalLayout { padding-left: 10px; padding-right: 10px; spacing: 8px; Heading { text: root.diagnostic-title != "" ? root.diagnostic-title : GuiI18n.text("gui-select-diagnostic"); font-size: 14px; } Rectangle { horizontal-stretch: 1; } TextField { text <=> root.diagnostic-query; placeholder: GuiI18n.text("gui-search-loaded-content"); input-type: InputType.search; width: 250px; accepted => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-find"); enabled: root.selected-diagnostic-name != ""; clicked => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-copy"); enabled: root.diagnostic-content != ""; clicked => { root.copy-text(root.diagnostic-content); } } AppButton { text: GuiI18n.text("gui-folder"); enabled: root.selected-diagnostic-name != ""; clicked => { root.open-diagnostic-folder(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } AppButton { text: GuiI18n.text("gui-refresh"); enabled: root.selected-diagnostic-name != ""; clicked => { root.refresh-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name); } } } }
                                    if root.diagnostic-content == "" : EmptyState { title: GuiI18n.text("gui-no-file-open"); message: GuiI18n.text("gui-no-file-open-detail"); }
                                    if root.diagnostic-content != "" : SelectableText { text: root.diagnostic-content; }
                                    if root.diagnostic-path != "" : Rectangle { height: 30px; background: Theme.surface; HorizontalLayout { padding-left: 10px; BodyText { text: root.diagnostic-path; color: Theme.text-muted; overflow: elide; } AppButton { text: GuiI18n.text("gui-copy-path"); min-width: 88px; height: 28px; clicked => { root.copy-text(root.diagnostic-path); } } } }
                                }
                            }
                        }
                    }
                }

                if root.page == 3 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 22px; spacing: 14px;
                        Heading { text: GuiI18n.text("gui-activity"); font-size: 25px; }
                        BodyText { text: GuiI18n.text("gui-activity-detail"); wrap: word-wrap; }
                        if root.activities.length == 0 : EmptyState { title: GuiI18n.text("gui-no-recent-activity"); message: GuiI18n.text("gui-no-recent-activity-detail"); }
                        ScrollView { VerticalLayout { spacing: 9px; for item in root.activities : Rectangle { height: 94px; border-radius: 8px; border-width: 1px; border-color: item.failed ? Theme.danger : item.active ? Theme.accent : Theme.border; background: Theme.surface; HorizontalLayout { padding: 13px; spacing: 13px; VerticalLayout { Heading { text: item.title; font-size: 14px; } BodyText { text: item.entity + (item.detail != "" ? " | " + item.detail : ""); } BodyText { text: item.id + " | " + item.time; color: Theme.text-muted; font-family: "monospace"; font-size: 11px; } if item.active && !item.indeterminate : Rectangle { height: 4px; border-radius: 2px; background: Theme.border; Rectangle { width: parent.width * item.progress; height: parent.height; border-radius: 2px; background: Theme.accent; } } } Pill { text: item.status; tone-color: item.failed ? Theme.danger : item.active ? Theme.accent : Theme.success; } if item.cancellable : AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.cancel-operation(item.id); } } } } } }
                    }
                }

                // Settings.
                if root.page == 4 : HorizontalLayout { spacing: 0px; horizontal-stretch: 1;
                    Rectangle { width: 190px; background: Theme.surface; border-width: 1px; border-color: Theme.border; VerticalLayout { padding: 14px; spacing: 5px; BodyText { text: GuiI18n.text("gui-settings-heading"); color: Theme.text-muted; font-size: 11px; }
                        AppButton { text: GuiI18n.text("gui-settings-general"); tone: root.workspace-tab == "General" ? "primary" : "secondary"; clicked => { root.workspace-tab = "General"; } }
                        AppButton { text: GuiI18n.text("gui-settings-minecraft"); tone: root.workspace-tab == "Minecraft" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Minecraft"; } }
                        AppButton { text: GuiI18n.text("gui-settings-java"); tone: root.workspace-tab == "Java" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Java"; } }
                        AppButton { text: GuiI18n.text("gui-settings-accounts"); tone: root.workspace-tab == "Accounts" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Accounts"; } }
                        AppButton { text: GuiI18n.text("gui-settings-providers"); tone: root.workspace-tab == "Providers & APIs" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Providers & APIs"; } }
                        AppButton { text: GuiI18n.text("gui-settings-storage"); tone: root.workspace-tab == "Storage" ? "primary" : "secondary"; clicked => { root.workspace-tab = "Storage"; } }
                        Rectangle { vertical-stretch: 1; } AppButton { text: GuiI18n.text("gui-settings-about"); tone: root.workspace-tab == "About & Licenses" ? "primary" : "secondary"; clicked => { root.workspace-tab = "About & Licenses"; } } } }
                    Rectangle { horizontal-stretch: 1; background: Theme.background; ScrollView { viewport-width: self.width; VerticalLayout { alignment: start; padding: 26px; spacing: 16px;
                        Heading { text: root.workspace-tab == "General" ? GuiI18n.text("gui-settings-general") : root.workspace-tab == "Minecraft" ? GuiI18n.text("gui-settings-minecraft") : root.workspace-tab == "Java" ? GuiI18n.text("gui-settings-java") : root.workspace-tab == "Accounts" ? GuiI18n.text("gui-settings-accounts") : root.workspace-tab == "Providers & APIs" ? GuiI18n.text("gui-settings-providers") : root.workspace-tab == "Storage" ? GuiI18n.text("gui-settings-storage") : GuiI18n.text("gui-settings-about"); font-size: 25px; }
                        if root.workspace-tab == "General" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-application-behavior"); } Rectangle { height: 88px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 15px; VerticalLayout { Heading { text: GuiI18n.text("gui-desktop-scale"); font-size: 14px; } BodyText { text: GuiI18n.text("gui-desktop-scale-detail"); wrap: word-wrap; } } Pill { text: GuiI18n.text("gui-system"); tone-color: Theme.accent-secondary; } } } }
                        if root.workspace-tab == "Minecraft" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-minecraft-defaults-detail"); wrap: word-wrap; }
                            Heading { text: GuiI18n.text("gui-display-gameplay-defaults"); font-size: 16px; }
                            for pref in root.preferences : Rectangle { height: 84px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; spacing: 12px; Toggle { checked: pref.enabled; toggled(value) => { root.save-preference(pref.key, pref.value, value); } } VerticalLayout { width: 250px; Heading { text: pref.label; font-size: 14px; } BodyText { text: pref.syntax; color: Theme.text-muted; } }
                                if pref.kind == "boolean" : HorizontalLayout { spacing: 9px; BodyText { text: pref.value == "true" ? GuiI18n.text("gui-enabled") : GuiI18n.text("gui-disabled"); } Toggle { checked: pref.value == "true"; enabled: pref.enabled; toggled(value) => { root.save-preference(pref.key, value ? "true" : "false", true); } } }
                                if pref.kind == "choice" : ComboBox { model: ["auto", "1", "2", "3", "4"]; enabled: pref.enabled; current-index: pref.value == "1" ? 1 : pref.value == "2" ? 2 : pref.value == "3" ? 3 : pref.value == "4" ? 4 : 0; selected(value) => { root.save-preference(pref.key, value, true); } width: 190px; }
                                if pref.kind == "slider" : HorizontalLayout { spacing: 9px; fov := Slider { enabled: pref.enabled; minimum: 30; maximum: 110; step: 1; value: pref.numeric-value; released(value) => { root.save-preference(pref.key, "" + round(value), true); } } BodyText { text: round(fov.value) == 70 ? GuiI18n.text("gui-fov-normal") : round(fov.value) == 110 ? GuiI18n.text("gui-fov-quake-pro") : round(fov.value) + " deg"; width: 90px; } }
                                if pref.kind == "integer" || pref.kind == "text" : HorizontalLayout { spacing: 8px; pref-editor := TextField { text: pref.value; enabled: pref.enabled; input-type: pref.kind == "integer" ? InputType.number : InputType.text; width: 230px; accepted => { root.save-preference(pref.key, self.text, true); } } AppButton { text: GuiI18n.text("gui-apply"); width: 92px; enabled: pref.enabled; clicked => { root.save-preference(pref.key, pref-editor.text, true); } } }
                            } }
                            BodyText { text: GuiI18n.text("gui-options-unavailable-detail"); color: Theme.text-muted; wrap: word-wrap; }
                        }
                        if root.workspace-tab == "Java" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-java-detail"); }
                            HorizontalLayout { spacing: 14px; LabeledToggle { label: GuiI18n.text("gui-java-detect"); checked <=> root.java-auto-detect; } LabeledToggle { label: GuiI18n.text("gui-java-download"); checked <=> root.java-auto-download; } Rectangle { horizontal-stretch: 1; } }
                            HorizontalLayout { spacing: 12px; VerticalLayout { FieldLabel { text: GuiI18n.text("gui-java-memory-min"); } TextField { text <=> root.java-memory-min; input-type: InputType.number; width: 190px; } } VerticalLayout { FieldLabel { text: GuiI18n.text("gui-java-memory-max"); } TextField { text <=> root.java-memory-max; input-type: InputType.number; width: 190px; } } }
                            Text { text: GuiI18n.text("gui-java-preferred"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.java-preferred-path; placeholder: "/usr/bin/java"; }
                            HorizontalLayout { AppButton { text: GuiI18n.text("gui-java-refresh"); clicked => { root.refresh-java(); } } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-java-save"); tone: "primary"; clicked => { root.save-java-settings(root.java-auto-detect, root.java-auto-download, root.java-memory-min, root.java-memory-max, root.java-preferred-path); } } }
                            for java in root.java-runtimes : Rectangle { height: 75px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; VerticalLayout { Heading { text: java.version + " | " + java.vendor; font-size: 14px; } BodyText { text: java.path; overflow: elide; } } Pill { text: java.origin; tone-color: Theme.accent-secondary; } } }
                        }
                        if root.workspace-tab == "Accounts" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-accounts-detail"); wrap: word-wrap; }
                            HorizontalLayout { spacing: 8px; TextField { text <=> root.offline-account-name; placeholder: GuiI18n.text("gui-offline-account-placeholder"); width: 260px; } AppButton { text: GuiI18n.text("gui-add-offline-account"); width: 180px; clicked => { root.add-offline-account(root.offline-account-name); } } AppButton { text: GuiI18n.text("gui-microsoft-sign-in"); width: 205px; tone: "primary"; clicked => { root.begin-microsoft-login(); } } Rectangle { horizontal-stretch: 1; } }
                            if root.accounts.length == 0 : EmptyState { title: GuiI18n.text("gui-no-accounts"); message: GuiI18n.text("gui-no-accounts-detail"); }
                            for account in root.accounts : Rectangle { height: 78px; border-radius: 8px; border-width: account.is-default ? 2px : 1px; border-color: account.is-default ? Theme.accent : Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; spacing: 8px; VerticalLayout { Heading { text: account.name; font-size: 14px; } BodyText { text: account.kind + " | " + account.credential-state; } } if account.is-default : Pill { text: GuiI18n.text("gui-default"); tone-color: Theme.accent; } if !account.is-default : AppButton { text: GuiI18n.text("gui-make-default"); width: 130px; clicked => { root.set-default-account(account.id); } } AppButton { text: GuiI18n.text("gui-remove"); width: 110px; tone: "danger"; clicked => { root.remove-account(account.id); } } } }
                        }
                        if root.workspace-tab == "Providers & APIs" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-provider-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-microsoft-client-id"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.microsoft-client-id; placeholder: GuiI18n.text("gui-client-id-placeholder"); } Text { text: GuiI18n.text("gui-user-agent"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.user-agent; placeholder: GuiI18n.text("gui-user-agent-placeholder"); } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-api-settings"); tone: "primary"; clicked => { root.save-api-settings(root.microsoft-client-id, root.user-agent); } } } BodyText { text: GuiI18n.text("gui-curseforge-unavailable"); color: Theme.text-muted; wrap: word-wrap; } }
                        if root.workspace-tab == "Storage" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-storage-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-storage-root"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.persistent-root; placeholder: root.storage-root; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-storage-root"); tone: "primary"; clicked => { root.save-storage-settings(root.persistent-root); } } } }
                        if root.workspace-tab == "About & Licenses" : VerticalLayout { alignment: start; spacing: 14px;
                            Image { source: root.logo; width: 92px; height: 92px; image-fit: contain; accessible-label: GuiI18n.text("gui-logo-accessible"); }
                            Heading { text: "Ephervex"; font-size: 24px; }
                            BodyText { text: GuiI18n.value("gui-version", root.app-version); }
                            BodyText { text: GuiI18n.text("gui-tagline"); }
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

        Timer { interval: root.toast-error ? 9000ms : 4500ms; running: root.toast-text != ""; triggered => { root.toast-text = ""; } }
        if root.toast-text != "" : Rectangle {
            x: parent.width - self.width - 22px;
            y: parent.height - self.height - 48px;
            width: min(440px, parent.width - 44px);
            height: max(72px, toast-message.preferred-height + 38px);
            border-radius: 8px;
            border-width: 1px;
            border-color: root.toast-error ? Theme.danger.with-alpha(0.75) : Theme.success.with-alpha(0.75);
            background: Theme.elevated;
            Rectangle { x: 0px; y: 0px; width: 4px; height: parent.height; background: root.toast-error ? Theme.danger : Theme.success; border-radius: 3px; }
            HorizontalLayout { padding-left: 16px; padding-right: 10px; padding-top: 10px; padding-bottom: 10px; spacing: 10px;
                VerticalLayout { spacing: 3px; Heading { text: root.toast-error ? GuiI18n.text("gui-notification-error") : GuiI18n.text("gui-notification-complete"); font-size: 13px; } toast-message := BodyText { text: root.toast-text; color: Theme.text-secondary; wrap: word-wrap; } }
                IconButton { text: "x"; width: 30px; height: 30px; accessible-role: button; accessible-label: GuiI18n.text("gui-dismiss-notification"); clicked => { root.toast-text = ""; } }
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
                    HorizontalLayout { Heading { text: root.modal == "create-instance" ? GuiI18n.text("gui-modal-new-instance") : root.modal == "import" ? GuiI18n.text("gui-modal-import") : root.modal == "create-server" ? GuiI18n.text("gui-modal-new-server") : root.modal == "mod-plan" ? GuiI18n.text("gui-modal-install-mod") : root.modal == "eula" ? GuiI18n.text("gui-server-eula") : root.modal == "kill" ? GuiI18n.text("gui-modal-force-kill") : root.modal == "delete-instance" ? GuiI18n.text("gui-delete-instance") : root.modal == "delete-server" ? GuiI18n.text("gui-delete-server") : root.modal == "license" ? GuiI18n.text("gui-modal-apache-license") : GuiI18n.text("gui-third-party-licenses"); font-size: 20px; } Rectangle { horizontal-stretch: 1; } IconButton { text: "x"; clicked => { root.modal = ""; root.modal-error = ""; } } }
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
