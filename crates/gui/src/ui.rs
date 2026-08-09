slint::slint! {
    import { AboutSlint, ComboBox, ScrollView, Slider, Spinner } from "std-widgets.slint";

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
        relationship: string,
        linked-client-id: string,
        state: string,
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
        height: 38px;
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
        callback accepted();
        callback edited(string);
        height: 38px;
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
            single-line: true;
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

    component Toggle inherits Rectangle {
        in-out property <bool> checked;
        in property <bool> enabled: true;
        callback toggled(bool);
        width: 42px;
        height: 23px;
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
        Rectangle { width: 3px; background: root.selected ? Theme.accent : transparent; border-radius: 2px; }
        HorizontalLayout {
            padding-left: 14px; padding-right: 12px; spacing: 13px;
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
            Image { source: @image-url("../../../assets/icons/environment-generic.svg"); width: 42px; height: 42px; image-fit: contain; }
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
                    Pill { text: item.storage; tone-color: item.storage == "Temporary" ? Theme.warning : Theme.success; }
                    if item.needs-attention : Pill { text: "ATTENTION"; tone-color: Theme.danger; }
                }
                BodyText { text: "Minecraft " + item.minecraft + " | " + item.loader + (item.mods > 0 ? " | " + item.mods + " managed mods" : ""); }
                BodyText { text: item.linked-servers > 0 ? item.linked-servers + (item.linked-servers == 1 ? " linked server" : " linked servers") : item.source + " instance"; color: item.linked-servers > 0 ? Theme.accent : Theme.text-muted; }
            }
            VerticalLayout {
                width: 150px; alignment: center; spacing: 7px;
                BodyText { text: item.updated; horizontal-alignment: center; }
                AppButton { text: item.can-launch ? "Launch" : "Launch unavailable"; tone: item.can-launch ? "primary" : "secondary"; enabled: item.can-launch; clicked => { root.launch(item.id); } }
            }
        }
        TouchArea { width: parent.width - 176px; clicked => { root.choose(item.id); } double-clicked => { root.open-workspace(item.id); } }
    }

    component ServerCard inherits Rectangle {
        in property <ServerRow> item;
        in property <bool> selected;
        callback choose(string);
        callback primary(string);
        callback open-workspace(string);
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
                    Pill { text: item.storage; tone-color: item.storage == "Temporary" ? Theme.warning : Theme.success; }
                    Pill { text: item.state; tone-color: item.state == "Running" ? Theme.success : item.state == "Stopped" ? Theme.text-secondary : Theme.warning; }
                }
                BodyText { text: "Minecraft " + item.minecraft + " | " + item.loader + " | " + item.relationship; }
                BodyText { text: item.address + " | " + item.state-detail; color: Theme.text-muted; }
            }
            VerticalLayout {
                width: 128px; alignment: center;
                AppButton {
                    text: item.can-start ? "Start" : item.can-stop ? "Stop" : "Unavailable";
                    tone: item.can-start ? "primary" : "secondary";
                    enabled: item.can-start || item.can-stop;
                    clicked => { root.primary(item.id); }
                }
            }
        }
        TouchArea { width: parent.width - 154px; clicked => { root.choose(item.id); } double-clicked => { root.open-workspace(item.id); } }
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
        if !has-selection : EmptyState { title: "No instance selected"; message: "Select an instance to inspect it."; }
        if has-selection : VerticalLayout {
            padding: 22px; spacing: 14px;
            HorizontalLayout { Heading { text: item.name; font-size: 18px; } AppButton { text: "Open"; clicked => { root.workspace(item.id); } } }
            Divider { }
            BodyText { text: "OVERVIEW"; color: Theme.text-muted; font-size: 11px; }
            HorizontalLayout { InspectorLabel { text: "Status"; } BodyText { text: item.needs-attention ? "Needs attention" : item.can-launch ? "Ready to launch" : "Busy"; color: item.needs-attention ? Theme.warning : Theme.success; } }
            HorizontalLayout { InspectorLabel { text: "Minecraft"; } BodyText { text: item.minecraft; } }
            HorizontalLayout { InspectorLabel { text: "Loader"; } BodyText { text: item.loader; } }
            HorizontalLayout { InspectorLabel { text: "Source"; } BodyText { text: item.source; } }
            HorizontalLayout { InspectorLabel { text: "Mods"; } BodyText { text: item.mods + " managed"; } }
            HorizontalLayout { InspectorLabel { text: "Storage"; } BodyText { text: item.storage; } }
            BodyText { text: item.path; color: Theme.text-muted; overflow: elide; }
            Divider { }
            BodyText { text: "ACTIONS"; color: Theme.text-muted; font-size: 11px; }
            AppButton { text: "Launch"; tone: "primary"; enabled: item.can-launch; clicked => { root.launch(item.id); } }
            if item.connect-server-id != "" : AppButton { text: "Launch & Connect to " + item.connect-server-name; enabled: item.can-connect; clicked => { root.connect(item.id, item.connect-server-id); } }
            AppButton { text: "Open Workspace"; clicked => { root.workspace(item.id); } }
            AppButton { text: "Create Linked Server"; clicked => { root.create-server(item.id); } }
            AppButton { text: "Open Folder"; clicked => { root.open-folder(item.id); } }
            Divider { }
            BodyText { text: "DIAGNOSTICS"; color: Theme.text-muted; font-size: 11px; }
            AppButton { text: "View Logs & Crash Reports"; clicked => { root.diagnostics(item.id); } }
        }
    }

    export component AppWindow inherits Window {
        title: "Ephervex";
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
        in-out property <string> selected-instance-id;
        in-out property <string> selected-server-id;
        in-out property <int> selected-instance-index: -1;
        in-out property <int> selected-server-index: -1;
        in-out property <string> instance-filter: "All";
        in-out property <string> instance-query;
        in-out property <string> server-query;
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
        in-out property <string> status-message: "Ready";
        in-out property <bool> loading: true;
        in-out property <int> active-operations;
        in-out property <string> storage-root;
        in-out property <string> app-version;
        in-out property <string> license-text;
        in-out property <string> toast-text;
        in-out property <bool> toast-error;
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
        callback select-instance(string);
        callback select-server(string);
        callback launch-client(string);
        callback launch-and-connect(string, string);
        callback open-client-folder(string);
        callback open-server-folder(string);
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
                height: 68px;
                background: #11171a;
                border-width: 1px;
                border-color: Theme.border;
                HorizontalLayout {
                    padding-left: 20px; padding-right: 20px; spacing: 14px;
                    Image { source: root.logo; width: 36px; height: 36px; image-fit: contain; }
                    Heading { text: "Ephervex"; font-size: 20px; width: 180px; }
                    Rectangle { horizontal-stretch: 1; }
                    if root.page == 0 && root.workspace == "" : AppButton { text: "Import Modpack"; clicked => { root.modal = "import"; } }
                    if root.page == 0 && root.workspace == "" : AppButton { text: "+  New Instance"; tone: "primary"; min-width: 148px; clicked => { root.modal = "create-instance"; } }
                    if root.page == 1 && root.workspace == "" : AppButton { text: "+  New Server"; tone: "primary"; min-width: 130px; clicked => { root.create-server-client-id = ""; root.modal = "create-server"; } }
                    AppButton { text: "Refresh"; clicked => { root.refresh(); } }
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
                        NavItem { icon: @image-url("../../../assets/icons/nav-instances.svg"); label: "Instances"; selected: root.page == 0; clicked => { root.page = 0; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-servers.svg"); label: "Servers"; selected: root.page == 1; clicked => { root.page = 1; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-logs.svg"); label: "Logs"; selected: root.page == 2; clicked => { root.page = 2; root.workspace = ""; root.load-diagnostics("launcher", "", "log"); } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-activity.svg"); label: "Activity"; selected: root.page == 3; clicked => { root.page = 3; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-settings.svg"); label: "Settings"; selected: root.page == 4; clicked => { root.page = 4; root.workspace = ""; root.workspace-tab = "General"; } }
                        Rectangle { vertical-stretch: 1; }
                        BodyText { text: "Spawn. Reproduce. Despawn."; color: Theme.text-muted; font-size: 11px; wrap: word-wrap; }
                    }
                }

                // Instances list and inspector.
                if root.page == 0 && root.workspace == "" : HorizontalLayout {
                    spacing: 0px;
                    Rectangle {
                        horizontal-stretch: 1;
                        background: Theme.background;
                        VerticalLayout {
                            padding: 22px; spacing: 16px;
                            HorizontalLayout { Heading { text: "Instances"; font-size: 25px; } Rectangle { horizontal-stretch: 1; } BodyText { text: root.instances.length + (root.instances.length == 1 ? " client instance" : " client instances"); } }
                            HorizontalLayout {
                                spacing: 8px;
                                AppButton { text: "All"; tone: root.instance-filter == "All" ? "primary" : "secondary"; min-width: 70px; clicked => { root.instance-filter = "All"; root.refresh(); } }
                                AppButton { text: "Temporary"; tone: root.instance-filter == "Temporary" ? "primary" : "secondary"; clicked => { root.instance-filter = "Temporary"; root.refresh(); } }
                                AppButton { text: "Persistent"; tone: root.instance-filter == "Persistent" ? "primary" : "secondary"; clicked => { root.instance-filter = "Persistent"; root.refresh(); } }
                                Rectangle { horizontal-stretch: 1; }
                                TextField { text <=> root.instance-query; placeholder: "Filter instances..."; width: 290px; accepted => { root.refresh(); } }
                            }
                            if root.loading : VerticalLayout { alignment: center; Spinner { width: 38px; height: 38px; } BodyText { text: "Loading instances..."; horizontal-alignment: center; } }
                            if !root.loading && root.instances.length == 0 : EmptyState {
                                title: "No client instances yet";
                                message: "Create an exact Vanilla, Forge, or NeoForge environment, or import a supported modpack.";
                                primary-text: "New Instance"; secondary-text: "Import Modpack";
                                primary => { root.modal = "create-instance"; }
                                secondary => { root.modal = "import"; }
                            }
                            if !root.loading && root.instances.length > 0 : ScrollView {
                                viewport-width: self.width;
                                VerticalLayout {
                                    spacing: 12px;
                                    for item[index] in root.instances : InstanceCard {
                                        visible: root.instance-filter == "All" || item.storage == root.instance-filter;
                                        item: item;
                                        selected: item.id == root.selected-instance-id;
                                        choose(id) => { root.selected-instance-id = id; root.select-instance(id); }
                                        launch(id) => { root.launch-client(id); }
                                        open-workspace(id) => { root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); }
                                    }
                                }
                            }
                        }
                    }
                    if root.show-inspector : InstanceInspector {
                        width: 350px;
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
                        HorizontalLayout { AppButton { text: "Back to Instances"; clicked => { root.workspace = ""; } } Heading { text: root.selected-instance-index >= 0 ? root.instances[root.selected-instance-index].name : "Instance"; font-size: 23px; } Rectangle { horizontal-stretch: 1; } AppButton { text: "Launch"; tone: "primary"; enabled: root.selected-instance-index >= 0 && root.instances[root.selected-instance-index].can-launch; clicked => { root.launch-client(root.selected-instance-id); } } }
                        HorizontalLayout {
                            spacing: 8px;
                            for tab in ["Overview", "Mods", "Minecraft Settings", "Logs"] : AppButton { text: tab; tone: root.workspace-tab == tab ? "primary" : "secondary"; clicked => { root.workspace-tab = tab; if tab == "Logs" { root.page = 2; root.load-diagnostics("client", root.selected-instance-id, "log"); } } }
                        }
                        Divider { }
                        if root.workspace-tab == "Overview" : ScrollView {
                            VerticalLayout { spacing: 12px;
                                Heading { text: "Reproduction Environment"; }
                                BodyText { text: "Exact target, retained source details, and safe actions for this client instance."; }
                                Rectangle { height: 126px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    HorizontalLayout { padding: 18px; spacing: 18px; LoaderGlyph { kind: root.instances[root.selected-instance-index].loader-kind; } VerticalLayout { Heading { text: root.instances[root.selected-instance-index].name; } BodyText { text: "Minecraft " + root.instances[root.selected-instance-index].minecraft + " | " + root.instances[root.selected-instance-index].loader; } BodyText { text: root.instances[root.selected-instance-index].storage + " | " + root.instances[root.selected-instance-index].source; } BodyText { text: root.instances[root.selected-instance-index].path; color: Theme.text-muted; } } }
                                }
                                HorizontalLayout { spacing: 8px; AppButton { text: "Open Folder"; clicked => { root.open-client-folder(root.selected-instance-id); } } AppButton { text: "Create Linked Server"; clicked => { root.create-server-client-id = root.selected-instance-id; root.modal = "create-server"; } } AppButton { text: "View Diagnostics"; clicked => { root.page = 2; root.load-diagnostics("client", root.selected-instance-id, "log"); } } }
                                BodyText { text: root.instances[root.selected-instance-index].storage == "Temporary" ? "Temporary instances remain until the operating system clears its temporary storage. Converting an existing instance in place is not available from the current application service." : "This instance is stored under the persistent instance root."; color: Theme.text-muted; wrap: word-wrap; }
                            }
                        }
                        if root.workspace-tab == "Mods" : HorizontalLayout {
                            spacing: 16px;
                            Rectangle { width: 42%; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                VerticalLayout { padding: 16px; spacing: 10px; Heading { text: "Managed Mods"; } BodyText { text: "Only mods installed through Ephervex are shown as managed."; wrap: word-wrap; }
                                    if root.managed-mods.length == 0 : EmptyState { title: "No managed mods"; message: "Manual JARs are not adopted or modified."; }
                                    ScrollView { VerticalLayout { spacing: 8px; for mod in root.managed-mods : Rectangle { height: 70px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; VerticalLayout { padding: 10px; Heading { text: mod.title; font-size: 14px; } BodyText { text: mod.version + " | " + mod.provider + " | " + mod.origin; } } } } }
                                }
                            }
                            Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                VerticalLayout { padding: 16px; spacing: 10px; Heading { text: "Add Mods from Modrinth"; } BodyText { text: "Search results are constrained to this instance's exact Minecraft version and loader."; wrap: word-wrap; }
                                    HorizontalLayout { TextField { text <=> root.server-query; placeholder: "Search compatible mods..."; horizontal-stretch: 1; accepted => { root.search-mods(root.selected-instance-id, root.server-query); } } AppButton { text: "Search"; tone: "primary"; clicked => { root.search-mods(root.selected-instance-id, root.server-query); } } }
                                    ScrollView { VerticalLayout { spacing: 8px; for mod in root.mod-results : Rectangle { height: 92px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; HorizontalLayout { padding: 10px; VerticalLayout { Heading { text: mod.title; font-size: 14px; } BodyText { text: mod.description; wrap: word-wrap; } BodyText { text: "by " + mod.author + " | " + mod.downloads + " downloads"; color: Theme.text-muted; } } AppButton { text: "Review"; clicked => { root.review-mod(root.selected-instance-id, mod.project-id); } } } } } }
                                }
                            }
                        }
                        if root.workspace-tab == "Minecraft Settings" : EmptyState { title: "Instance-owned Minecraft settings"; message: "The current core safely applies known Vanilla defaults when an instance is created, but does not expose a typed editor for an existing instance. Unknown and mod-added options remain untouched."; primary-text: "Open Global Defaults"; primary => { root.page = 4; root.workspace = ""; root.workspace-tab = "Minecraft"; } }
                    }
                }

                // Servers.
                if root.page == 1 && root.workspace == "" : HorizontalLayout {
                    spacing: 0px;
                    Rectangle { horizontal-stretch: 1; background: Theme.background;
                        VerticalLayout { padding: 22px; spacing: 16px;
                            HorizontalLayout { Heading { text: "Servers"; font-size: 25px; } Rectangle { horizontal-stretch: 1; } TextField { text <=> root.server-query; placeholder: "Filter servers..."; width: 290px; accepted => { root.refresh(); } } }
                            if root.servers.length == 0 : EmptyState { title: "No servers yet"; message: "Create a standalone exact server or create one linked to a client reproduction environment."; primary-text: "New Server"; primary => { root.create-server-client-id = ""; root.modal = "create-server"; } }
                            if root.servers.length > 0 : ScrollView { viewport-width: self.width; VerticalLayout { spacing: 12px; for item in root.servers : ServerCard { item: item; selected: item.id == root.selected-server-id; choose(id) => { root.selected-server-id = id; root.select-server(id); } primary(id) => { if item.can-start { root.server-start(id); } else { root.server-stop(id); } } open-workspace(id) => { root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(id); } } } }
                        }
                    }
                    if root.show-inspector : Rectangle { width: 350px; background: Theme.surface; border-width: 1px; border-color: Theme.border;
                        if root.selected-server-index < 0 : EmptyState { title: "No server selected"; message: "Select a server to inspect its runtime."; }
                        if root.selected-server-index >= 0 : VerticalLayout { padding: 22px; spacing: 13px;
                            Heading { text: root.servers[root.selected-server-index].name; }
                            Divider { }
                            BodyText { text: "OVERVIEW"; color: Theme.text-muted; font-size: 11px; }
                            HorizontalLayout { InspectorLabel { text: "State"; } BodyText { text: root.servers[root.selected-server-index].state; color: root.servers[root.selected-server-index].state == "Running" ? Theme.success : Theme.text-secondary; } }
                            HorizontalLayout { InspectorLabel { text: "Target"; } BodyText { text: root.servers[root.selected-server-index].minecraft + " | " + root.servers[root.selected-server-index].loader; } }
                            HorizontalLayout { InspectorLabel { text: "Relation"; } BodyText { text: root.servers[root.selected-server-index].relationship; } }
                            HorizontalLayout { InspectorLabel { text: "Address"; } BodyText { text: root.servers[root.selected-server-index].address; } }
                            Divider { }
                            BodyText { text: "ACTIONS"; color: Theme.text-muted; font-size: 11px; }
                            AppButton { text: root.servers[root.selected-server-index].can-start ? "Start" : "Stop"; tone: root.servers[root.selected-server-index].can-start ? "primary" : "secondary"; enabled: root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop; clicked => { if root.servers[root.selected-server-index].can-start { root.server-start(root.selected-server-id); } else { root.server-stop(root.selected-server-id); } } }
                            AppButton { text: "Open Server Workspace"; clicked => { root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(root.selected-server-id); } }
                            if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: "Launch Client & Connect"; clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } }
                            AppButton { text: "Open Folder"; clicked => { root.open-server-folder(root.selected-server-id); } }
                            Divider { }
                            AppButton { text: "Force Kill..."; tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } }
                        }
                    }
                }

                if root.page == 1 && root.workspace == "server" : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 22px; spacing: 14px;
                        HorizontalLayout { AppButton { text: "Back to Servers"; clicked => { root.workspace = ""; } } Heading { text: root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : "Server"; font-size: 23px; } Rectangle { horizontal-stretch: 1; } AppButton { text: "Restart"; enabled: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-stop ? "Stop" : "Start"; tone: "primary"; enabled: root.selected-server-index >= 0 && (root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop); clicked => { if root.servers[root.selected-server-index].can-stop { root.server-stop(root.selected-server-id); } else { root.server-start(root.selected-server-id); } } } }
                        HorizontalLayout { spacing: 8px; for tab in ["Overview", "Console", "Properties", "Logs"] : AppButton { text: tab; tone: root.workspace-tab == tab ? "primary" : "secondary"; clicked => { root.workspace-tab = tab; if tab == "Logs" { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } } } }
                        Divider { }
                        if root.workspace-tab == "Overview" : ScrollView { VerticalLayout { spacing: 12px; Heading { text: "Dedicated Server"; } BodyText { text: root.servers[root.selected-server-index].state-detail; } Rectangle { height: 160px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 18px; spacing: 7px; BodyText { text: "Exact target    Minecraft " + root.servers[root.selected-server-index].minecraft + " | " + root.servers[root.selected-server-index].loader; } BodyText { text: "Runtime         " + root.servers[root.selected-server-index].state + " - " + root.servers[root.selected-server-index].state-detail; } BodyText { text: "Network         " + root.servers[root.selected-server-index].address; } BodyText { text: "Storage         " + root.servers[root.selected-server-index].storage; } BodyText { text: "Relationship    " + root.servers[root.selected-server-index].relationship; } } } HorizontalLayout { spacing: 8px; AppButton { text: "Open Folder"; clicked => { root.open-server-folder(root.selected-server-id); } } AppButton { text: "View Logs"; clicked => { root.page = 2; root.load-diagnostics("server", root.selected-server-id, "log"); } } if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: "Launch Client & Connect"; tone: "primary"; clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } } } } }
                        if root.workspace-tab == "Console" : VerticalLayout { spacing: 10px;
                            HorizontalLayout { BodyText { text: root.servers[root.selected-server-index].state + " | bounded retained output"; color: root.servers[root.selected-server-index].can-send-command ? Theme.success : Theme.text-secondary; } Rectangle { horizontal-stretch: 1; } AppButton { text: "Restart"; enabled: root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: "Stop"; enabled: root.servers[root.selected-server-index].can-stop; clicked => { root.server-stop(root.selected-server-id); } } AppButton { text: "Kill..."; tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } } }
                            Rectangle { vertical-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: rgb(11, 14, 16); ScrollView { VerticalLayout { padding: 12px; spacing: 2px; for line in root.console-lines : Text { text: line; color: Theme.text-secondary; font-family: "monospace"; font-size: 12px; wrap: no-wrap; } } } }
                            HorizontalLayout { TextField { text <=> root.console-command; placeholder: root.servers[root.selected-server-index].can-send-command ? "Enter server command..." : "Start the server to send commands"; enabled: root.servers[root.selected-server-index].can-send-command; horizontal-stretch: 1; accepted => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } AppButton { text: "Send"; tone: "primary"; enabled: root.servers[root.selected-server-index].can-send-command && root.console-command != ""; clicked => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } }
                        }
                        if root.workspace-tab == "Properties" : ScrollView { VerticalLayout { spacing: 14px;
                            Heading { text: "Network"; } BodyText { text: "Changes are validated and saved explicitly. A custom bind accepts an IP address, not a hostname."; wrap: word-wrap; }
                            HorizontalLayout { spacing: 12px; VerticalLayout { Text { text: "Access"; color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["Localhost", "LAN", "Custom"]; current-index: root.property-access == "LAN" ? 1 : root.property-access == "Custom" ? 2 : 0; selected(value) => { root.property-access = value; root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } width: 190px; } } VerticalLayout { Text { text: "Port"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.property-port; placeholder: "25565"; invalid: root.property-port-error != ""; width: 150px; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } if root.property-port-error != "" : Text { text: root.property-port-error; color: Theme.danger; font-size: 11px; } } VerticalLayout { horizontal-stretch: 1; Text { text: "Custom bind address"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.property-address; placeholder: "127.0.0.1 or ::1"; enabled: root.property-access == "Custom"; invalid: root.property-address-error != ""; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } if root.property-address-error != "" : Text { text: root.property-address-error; color: Theme.danger; font-size: 11px; } } }
                            HorizontalLayout { spacing: 10px; BodyText { text: "Online mode"; } Toggle { checked <=> root.property-online-mode; } if !root.property-online-mode && root.property-access != "Localhost" : BodyText { text: "Warning: Offline-mode access beyond localhost is unsafe."; color: Theme.warning; } }
                            Divider { } Heading { text: "Gameplay"; }
                            HorizontalLayout { spacing: 12px; VerticalLayout { horizontal-stretch: 1; Text { text: "Message of the day"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.property-motd; placeholder: "A Minecraft Server"; } } VerticalLayout { width: 190px; Text { text: "Maximum players"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.property-max-players; placeholder: "20"; invalid: root.property-max-players-error != ""; edited => { root.validate-server-properties(root.property-access, root.property-address, root.property-port, root.property-max-players); } } if root.property-max-players-error != "" : Text { text: root.property-max-players-error; color: Theme.danger; font-size: 11px; } } }
                            HorizontalLayout { spacing: 12px; VerticalLayout { Text { text: "Difficulty"; color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["peaceful", "easy", "normal", "hard"]; current-index: root.property-difficulty == "peaceful" ? 0 : root.property-difficulty == "easy" ? 1 : root.property-difficulty == "hard" ? 3 : 2; selected(value) => { root.property-difficulty = value; } width: 240px; } } VerticalLayout { Text { text: "Game mode"; color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["survival", "creative", "adventure", "spectator"]; current-index: root.property-gamemode == "creative" ? 1 : root.property-gamemode == "adventure" ? 2 : root.property-gamemode == "spectator" ? 3 : 0; selected(value) => { root.property-gamemode = value; } width: 270px; } } }
                            HorizontalLayout { spacing: 22px; BodyText { text: "Allow flight"; } Toggle { checked <=> root.property-allow-flight; } BodyText { text: "PvP"; } Toggle { checked <=> root.property-pvp; } BodyText { text: "Allowlist"; } Toggle { checked <=> root.property-allowlist; } }
                            Rectangle { height: 84px; border-radius: 8px; border-width: 1px; border-color: root.eula-accepted ? Theme.success : Theme.warning; background: Theme.surface; HorizontalLayout { padding: 14px; VerticalLayout { Heading { text: "Minecraft Server EULA"; font-size: 14px; } BodyText { text: root.eula-status; wrap: word-wrap; } } AppButton { text: root.eula-accepted ? "Review" : "Review & Accept"; clicked => { root.modal = "eula"; } } } }
                            HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Save Changes"; tone: "primary"; enabled: root.servers[root.selected-server-index].state == "Stopped"; clicked => { root.save-server-properties(root.selected-server-id, root.property-motd, root.property-max-players, root.property-difficulty, root.property-gamemode, root.property-allow-flight, root.property-pvp, root.property-allowlist, root.property-online-mode, root.property-access, root.property-address, root.property-port); } } }
                        } }
                    }
                }

                // Logs and crash reports.
                if root.page == 2 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 20px; spacing: 12px;
                        HorizontalLayout { Heading { text: "Logs & Crash Reports"; font-size: 25px; } Rectangle { horizontal-stretch: 1; } AppButton { text: "Client"; clicked => { if root.selected-instance-id != "" { root.load-diagnostics("client", root.selected-instance-id, "log"); } } } AppButton { text: "Server"; clicked => { if root.selected-server-id != "" { root.load-diagnostics("server", root.selected-server-id, "log"); } } } AppButton { text: "Launcher"; tone: root.selected-diagnostic-scope == "launcher" ? "primary" : "secondary"; clicked => { root.load-diagnostics("launcher", "", "log"); } } }
                        HorizontalLayout { spacing: 12px;
                            Rectangle { width: 300px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 10px; spacing: 7px;
                                HorizontalLayout { BodyText { text: root.diagnostics.length + " diagnostic files"; } Rectangle { horizontal-stretch: 1; } AppButton { text: "Refresh"; clicked => { root.load-diagnostics(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } }
                                if root.diagnostics.length == 0 : EmptyState { title: "No diagnostics"; message: "This source has not written any matching files."; }
                                ScrollView { VerticalLayout { spacing: 5px; for item in root.diagnostics : Rectangle { height: 76px; border-radius: 6px; border-width: item.name == root.selected-diagnostic-name ? 2px : 1px; border-color: item.name == root.selected-diagnostic-name ? Theme.accent : Theme.border; background: Theme.background; VerticalLayout { padding: 8px; Text { text: (item.kind == "crash-report" ? "Crash: " : "Log: ") + item.name; color: item.kind == "crash-report" ? Theme.warning : Theme.text; font-size: 13px; overflow: elide; } BodyText { text: item.entity + " | " + item.modified; font-size: 11px; } BodyText { text: item.summary != "" ? item.summary : item.size; color: Theme.text-muted; font-size: 11px; overflow: elide; } } TouchArea { clicked => { root.selected-diagnostic-name = item.name; root.selected-diagnostic-scope = item.scope-kind; root.selected-diagnostic-id = item.scope-id; root.selected-diagnostic-kind = item.kind; root.open-diagnostic(item.scope-kind, item.scope-id, item.kind, item.name); } } } } }
                            } }
                            Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: rgb(11, 14, 16);
                                VerticalLayout { spacing: 0px;
                                    Rectangle { height: 52px; background: Theme.surface; HorizontalLayout { padding-left: 10px; padding-right: 10px; spacing: 8px; Heading { text: root.diagnostic-title != "" ? root.diagnostic-title : "Select a diagnostic file"; font-size: 14px; } Rectangle { horizontal-stretch: 1; } TextField { text <=> root.diagnostic-query; placeholder: "Search loaded content..."; width: 250px; accepted => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: "Find"; enabled: root.selected-diagnostic-name != ""; clicked => { root.search-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: "Copy"; enabled: root.diagnostic-content != ""; clicked => { root.copy-text(root.diagnostic-content); } } AppButton { text: "Folder"; enabled: root.selected-diagnostic-name != ""; clicked => { root.open-diagnostic-folder(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind); } } AppButton { text: "Refresh"; enabled: root.selected-diagnostic-name != ""; clicked => { root.refresh-diagnostic(root.selected-diagnostic-scope, root.selected-diagnostic-id, root.selected-diagnostic-kind, root.selected-diagnostic-name); } } } }
                                    if root.diagnostic-content == "" : EmptyState { title: "No file open"; message: "Choose a log or crash report. Ephervex reads a bounded, safe text window through diagnostics services."; }
                                    if root.diagnostic-content != "" : ScrollView { Text { text: root.diagnostic-content; color: Theme.text-secondary; font-family: "monospace"; font-size: 12px; wrap: no-wrap; }
                                    }
                                    if root.diagnostic-path != "" : Rectangle { height: 30px; background: Theme.surface; HorizontalLayout { padding-left: 10px; BodyText { text: root.diagnostic-path; color: Theme.text-muted; overflow: elide; } AppButton { text: "Copy Path"; min-width: 88px; height: 28px; clicked => { root.copy-text(root.diagnostic-path); } } } }
                                }
                            }
                        }
                    }
                }

                if root.page == 3 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                    VerticalLayout { padding: 22px; spacing: 14px;
                        Heading { text: "Activity"; font-size: 25px; }
                        BodyText { text: "Current and recent work while Ephervex is running. Activity is not persisted across application restarts."; wrap: word-wrap; }
                        if root.activities.length == 0 : EmptyState { title: "No recent activity"; message: "Creation, imports, installations, launches, and server work will appear here."; }
                        ScrollView { VerticalLayout { spacing: 9px; for item in root.activities : Rectangle { height: 94px; border-radius: 8px; border-width: 1px; border-color: item.failed ? Theme.danger : item.active ? Theme.accent : Theme.border; background: Theme.surface; HorizontalLayout { padding: 13px; spacing: 13px; VerticalLayout { Heading { text: item.title; font-size: 14px; } BodyText { text: item.entity + (item.detail != "" ? " | " + item.detail : ""); } BodyText { text: item.id + " | " + item.time; color: Theme.text-muted; font-family: "monospace"; font-size: 11px; } if item.active && !item.indeterminate : Rectangle { height: 4px; border-radius: 2px; background: Theme.border; Rectangle { width: parent.width * item.progress; height: parent.height; border-radius: 2px; background: Theme.accent; } } } Pill { text: item.status; tone-color: item.failed ? Theme.danger : item.active ? Theme.accent : Theme.success; } if item.cancellable : AppButton { text: "Cancel"; clicked => { root.cancel-operation(item.id); } } } } } }
                    }
                }

                // Settings.
                if root.page == 4 : HorizontalLayout { spacing: 0px;
                    Rectangle { width: 190px; background: Theme.surface; border-width: 1px; border-color: Theme.border; VerticalLayout { padding: 14px; spacing: 5px; BodyText { text: "SETTINGS"; color: Theme.text-muted; font-size: 11px; } for tab in ["General", "Minecraft", "Java", "Accounts", "Providers & APIs", "Storage"] : AppButton { text: tab; tone: root.workspace-tab == tab ? "primary" : "secondary"; clicked => { root.workspace-tab = tab; } } Rectangle { vertical-stretch: 1; } AppButton { text: "About & Licenses"; tone: root.workspace-tab == "About & Licenses" ? "primary" : "secondary"; clicked => { root.workspace-tab = "About & Licenses"; } } } }
                    Rectangle { horizontal-stretch: 1; background: Theme.background; ScrollView { viewport-width: self.width; VerticalLayout { alignment: start; padding: 26px; spacing: 16px;
                        Heading { text: root.workspace-tab; font-size: 25px; }
                        if root.workspace-tab == "General" : VerticalLayout { spacing: 12px; BodyText { text: "Application behavior"; } Rectangle { height: 88px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 15px; VerticalLayout { Heading { text: "Desktop scale"; font-size: 14px; } BodyText { text: "Ephervex follows the desktop scale reported by Slint and your Linux environment."; wrap: word-wrap; } } Pill { text: "SYSTEM"; tone-color: Theme.accent-secondary; } } } }
                        if root.workspace-tab == "Minecraft" : VerticalLayout { spacing: 12px; BodyText { text: "Known Vanilla defaults are applied once when a client is created or imported. Existing instances and unknown/mod-added options are not synchronized or modified."; wrap: word-wrap; }
                            Heading { text: "Display & Gameplay Defaults"; font-size: 16px; }
                            for pref in root.preferences : Rectangle { height: 84px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; spacing: 12px; Toggle { checked: pref.enabled; toggled(value) => { root.save-preference(pref.key, pref.value, value); } } VerticalLayout { width: 250px; Heading { text: pref.label; font-size: 14px; } BodyText { text: pref.syntax; color: Theme.text-muted; } }
                                if pref.kind == "boolean" : HorizontalLayout { spacing: 9px; BodyText { text: pref.value == "true" ? "Enabled" : "Disabled"; } Toggle { checked: pref.value == "true"; enabled: pref.enabled; toggled(value) => { root.save-preference(pref.key, value ? "true" : "false", true); } } }
                                if pref.kind == "choice" : ComboBox { model: ["auto", "1", "2", "3", "4"]; enabled: pref.enabled; current-index: pref.value == "1" ? 1 : pref.value == "2" ? 2 : pref.value == "3" ? 3 : pref.value == "4" ? 4 : 0; selected(value) => { root.save-preference(pref.key, value, true); } width: 190px; }
                                if pref.kind == "slider" : HorizontalLayout { spacing: 9px; fov := Slider { enabled: pref.enabled; minimum: 30; maximum: 110; step: 1; value: pref.numeric-value; released(value) => { root.save-preference(pref.key, "" + round(value), true); } } BodyText { text: round(fov.value) + "°"; width: 48px; } }
                                if pref.kind == "integer" || pref.kind == "text" : HorizontalLayout { spacing: 8px; pref-editor := TextField { text: pref.value; enabled: pref.enabled; width: 230px; accepted => { root.save-preference(pref.key, self.text, true); } } AppButton { text: "Apply"; enabled: pref.enabled; clicked => { root.save-preference(pref.key, pref-editor.text, true); } } }
                            } }
                            BodyText { text: "Sound & Music and key bindings are not shown because the current core does not expose safe typed mappings for them. Ephervex never guesses schemas for options.txt."; color: Theme.text-muted; wrap: word-wrap; }
                        }
                        if root.workspace-tab == "Java" : VerticalLayout { spacing: 12px; BodyText { text: "Runtime discovery and JVM defaults are owned by the application services."; }
                            HorizontalLayout { spacing: 18px; BodyText { text: "Detect installed Java runtimes"; } Toggle { checked <=> root.java-auto-detect; } BodyText { text: "Download a managed runtime when needed"; } Toggle { checked <=> root.java-auto-download; } }
                            HorizontalLayout { spacing: 12px; VerticalLayout { Text { text: "Minimum memory (MiB)"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.java-memory-min; width: 190px; } } VerticalLayout { Text { text: "Maximum memory (MiB)"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.java-memory-max; width: 190px; } } }
                            Text { text: "Preferred Java executable (leave empty for automatic)"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.java-preferred-path; placeholder: "/usr/bin/java"; }
                            HorizontalLayout { AppButton { text: "Refresh Runtimes"; clicked => { root.refresh-java(); } } Rectangle { horizontal-stretch: 1; } AppButton { text: "Save Java Settings"; tone: "primary"; clicked => { root.save-java-settings(root.java-auto-detect, root.java-auto-download, root.java-memory-min, root.java-memory-max, root.java-preferred-path); } } }
                            for java in root.java-runtimes : Rectangle { height: 75px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; VerticalLayout { Heading { text: java.version + " | " + java.vendor; font-size: 14px; } BodyText { text: java.path; overflow: elide; } } Pill { text: java.origin; tone-color: Theme.accent-secondary; } } }
                        }
                        if root.workspace-tab == "Accounts" : VerticalLayout { spacing: 12px; BodyText { text: "Access tokens and refresh credentials are never displayed. Microsoft credentials use the desktop keyring when available."; wrap: word-wrap; }
                            HorizontalLayout { TextField { text <=> root.offline-account-name; placeholder: "Offline player name"; width: 260px; } AppButton { text: "Add Offline Account"; clicked => { root.add-offline-account(root.offline-account-name); } } AppButton { text: "Sign in with Microsoft"; tone: "primary"; clicked => { root.begin-microsoft-login(); } } }
                            if root.accounts.length == 0 : EmptyState { title: "No accounts"; message: "Add an offline identity for local testing or sign in to launch authenticated Minecraft."; }
                            for account in root.accounts : Rectangle { height: 78px; border-radius: 8px; border-width: account.is-default ? 2px : 1px; border-color: account.is-default ? Theme.accent : Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; VerticalLayout { Heading { text: account.name; font-size: 14px; } BodyText { text: account.kind + " | " + account.credential-state; } } if account.is-default : Pill { text: "DEFAULT"; tone-color: Theme.accent; } if !account.is-default : AppButton { text: "Make Default"; clicked => { root.set-default-account(account.id); } } AppButton { text: "Remove..."; tone: "danger"; clicked => { root.remove-account(account.id); } } } }
                        }
                        if root.workspace-tab == "Providers & APIs" : VerticalLayout { spacing: 12px; BodyText { text: "Provider configuration remains scoped. Stored secrets are reported only as present or absent."; wrap: word-wrap; } Text { text: "Microsoft application Client ID"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.microsoft-client-id; placeholder: "Use Ephervex build default"; } Text { text: "Custom HTTP User-Agent"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.user-agent; placeholder: "Use Ephervex default"; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Save API Settings"; tone: "primary"; clicked => { root.save-api-settings(root.microsoft-client-id, root.user-agent); } } } BodyText { text: "CurseForge API credentials are kept in the desktop keyring and are never revealed after storage. Credential editing will be enabled when the GUI core boundary exposes a secure secret prompt."; color: Theme.text-muted; wrap: word-wrap; } }
                        if root.workspace-tab == "Storage" : VerticalLayout { spacing: 12px; BodyText { text: "Temporary instances use the operating system's temporary directory. Ephervex does not delete them when Minecraft exits; the operating system owns cleanup."; wrap: word-wrap; } Text { text: "Persistent instance root (leave empty for the XDG default)"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.persistent-root; placeholder: root.storage-root; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Save Storage Root"; tone: "primary"; clicked => { root.save-storage-settings(root.persistent-root); } } } }
                        if root.workspace-tab == "About & Licenses" : VerticalLayout { alignment: start; spacing: 14px;
                            Image { source: root.logo; width: 92px; height: 92px; image-fit: contain; accessible-label: "Ephervex application logo"; }
                            Heading { text: "Ephervex"; font-size: 24px; }
                            BodyText { text: "Version " + root.app-version; }
                            BodyText { text: "Spawn. Reproduce. Despawn."; }
                            BodyText { text: "Licensed under the Apache License 2.0"; color: Theme.text; }
                            HorizontalLayout { AppButton { text: "View Ephervex License"; clicked => { root.modal = "license"; } } Rectangle { horizontal-stretch: 1; } }
                            Divider { }
                            Heading { text: "Built with Slint"; }
                            Rectangle { width: 290px; height: 105px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; AboutSlint { } }
                            Divider { }
                            Heading { text: "Third-Party Software"; }
                            BodyText { text: "Ephervex depends on open-source software with its own license terms."; }
                            HorizontalLayout { AppButton { text: "Third-Party Licenses"; clicked => { root.modal = "third-party"; } } Rectangle { horizontal-stretch: 1; } }
                        }
                    } } }
                }
            }

            Rectangle { height: 34px; background: #11171a; border-width: 1px; border-color: Theme.border; HorizontalLayout { padding-left: 14px; padding-right: 14px; spacing: 22px; BodyText { text: root.active-operations > 0 ? root.active-operations + " active operations" : "No active tasks"; color: root.active-operations > 0 ? Theme.accent : Theme.success; font-size: 11px; } Rectangle { horizontal-stretch: 1; } BodyText { text: root.status-message; font-size: 11px; } BodyText { text: root.storage-root; color: Theme.text-muted; font-size: 11px; overflow: elide; width: 300px; } }
            }
        }

        // Toast feedback.
        if root.toast-text != "" : Rectangle { x: parent.width - self.width - 22px; y: parent.height - self.height - 48px; width: min(430px, message.preferred-width + 42px); height: max(48px, message.preferred-height + 22px); border-radius: 7px; border-width: 1px; border-color: root.toast-error ? Theme.danger : Theme.success; background: Theme.elevated; message := Text { x: 14px; y: 10px; width: parent.width - 28px; height: parent.height - 20px; text: root.toast-text; color: Theme.text; font-size: 13px; wrap: word-wrap; vertical-alignment: center; } TouchArea { accessible-role: button; accessible-label: "Dismiss notification"; clicked => { root.toast-text = ""; } } }

        // Modal overlay and focused workflows.
        if root.modal != "" : Rectangle {
            background: #00000099;
            TouchArea { clicked => { } }
            Rectangle {
                width: root.modal == "license" ? min(820px, parent.width - 80px) : min(620px, parent.width - 80px);
                height: root.modal == "license" ? min(720px, parent.height - 80px) : root.modal == "third-party" ? 340px : root.modal == "mod-plan" ? 430px : root.modal == "eula" ? 440px : root.modal == "kill" ? 300px : 570px;
                x: (parent.width - self.width) / 2;
                y: (parent.height - self.height) / 2;
                border-radius: 8px;
                border-width: 1px;
                border-color: Theme.border;
                background: Theme.elevated;
                VerticalLayout { padding: 22px; spacing: 13px;
                    HorizontalLayout { Heading { text: root.modal == "create-instance" ? "New Instance" : root.modal == "import" ? "Import Modpack" : root.modal == "create-server" ? "New Server" : root.modal == "mod-plan" ? "Install Mod" : root.modal == "eula" ? "Minecraft Server EULA" : root.modal == "kill" ? "Force Kill Server" : root.modal == "license" ? "Apache License 2.0" : "Third-Party Licenses"; font-size: 20px; } Rectangle { horizontal-stretch: 1; } IconButton { text: "x"; clicked => { root.modal = ""; root.modal-error = ""; } } }
                    Divider { }
                    if root.modal == "create-instance" : VerticalLayout { spacing: 11px; BodyText { text: "Create an exact client environment. Temporary storage is the default."; wrap: word-wrap; } Text { text: "Name"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-name; placeholder: "Reproduction environment"; } Text { text: "Minecraft release"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-minecraft; placeholder: "1.21.1"; } Text { text: "Loader"; color: Theme.text-secondary; font-size: 12px; } ComboBox { model: ["Vanilla", "Forge", "NeoForge"]; current-index: root.create-loader == "Forge" ? 1 : root.create-loader == "NeoForge" ? 2 : 0; selected(value) => { root.create-loader = value; } } if root.create-loader != "Vanilla" : TextField { text <=> root.create-loader-version; placeholder: "Exact loader version"; } HorizontalLayout { BodyText { text: "Store persistently"; } Toggle { checked <=> root.create-persistent; } BodyText { text: root.create-persistent ? "Persistent instance root" : "Operating system temporary directory"; color: Theme.text-muted; } } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Cancel"; clicked => { root.modal = ""; } } AppButton { text: "Create Instance"; tone: "primary"; enabled: root.create-name != "" && root.create-minecraft != "" && (root.create-loader == "Vanilla" || root.create-loader-version != ""); clicked => { root.submit-create-instance(root.create-name, root.create-minecraft, root.create-loader, root.create-loader-version, root.create-persistent); } } } }
                    if root.modal == "import" : VerticalLayout { spacing: 11px; BodyText { text: "Supported archives are inspected and imported through Ephervex application services. Imported commands and unsafe paths are never executed."; wrap: word-wrap; } Text { text: "Modpack archive"; color: Theme.text-secondary; font-size: 12px; } HorizontalLayout { TextField { text <=> root.import-path; placeholder: "Choose a .mrpack or supported .zip"; horizontal-stretch: 1; } AppButton { text: "Browse..."; clicked => { root.import-analysis = ""; root.choose-import-file(); } } } if root.import-analysis != "" : Rectangle { height: 64px; border-radius: 6px; border-width: 1px; border-color: Theme.success; background: Theme.surface; BodyText { x: 12px; width: parent.width - 24px; text: "Validated: " + root.import-analysis; wrap: word-wrap; } } Text { text: "Instance name (optional)"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.import-name; placeholder: "Use the detected pack name"; } HorizontalLayout { BodyText { text: "Store persistently"; } Toggle { checked <=> root.create-persistent; } } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Cancel"; clicked => { root.modal = ""; } } AppButton { text: "Import Modpack"; tone: "primary"; enabled: root.import-path != ""; clicked => { root.submit-import(root.import-path, root.import-name, root.create-persistent); } } } }
                    if root.modal == "create-server" : ScrollView { VerticalLayout { spacing: 10px; BodyText { text: root.create-server-client-id != "" ? "Create and materialize a server linked to the selected client's retained recipe." : "Create and install a standalone exact server."; wrap: word-wrap; } Text { text: "Name"; color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-server-name; placeholder: "Test server"; } if root.create-server-client-id == "" : Text { text: "Minecraft release"; color: Theme.text-secondary; font-size: 12px; } if root.create-server-client-id == "" : TextField { text <=> root.create-server-minecraft; placeholder: "1.21.1"; } if root.create-server-client-id == "" : Text { text: "Loader"; color: Theme.text-secondary; font-size: 12px; } if root.create-server-client-id == "" : ComboBox { model: ["Vanilla", "Forge", "NeoForge"]; current-index: root.create-server-loader == "Forge" ? 1 : root.create-server-loader == "NeoForge" ? 2 : 0; selected(value) => { root.create-server-loader = value; } } if root.create-server-client-id == "" && root.create-server-loader != "Vanilla" : TextField { text <=> root.create-server-loader-version; placeholder: "Exact loader version"; } HorizontalLayout { BodyText { text: "Store persistently"; } Toggle { checked <=> root.create-server-persistent; } } BodyText { text: "EULA consent is not implied. Starting is blocked until you explicitly review and accept it."; color: Theme.warning; wrap: word-wrap; } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Cancel"; clicked => { root.modal = ""; } } AppButton { text: "Create Server"; tone: "primary"; enabled: root.create-server-name != ""; clicked => { root.submit-create-server(root.create-server-name, root.create-server-client-id, root.create-server-minecraft, root.create-server-loader, root.create-server-loader-version, root.create-server-persistent); } } } } }
                    if root.modal == "mod-plan" : VerticalLayout { spacing: 12px; Heading { text: root.mod-plan-title + " " + root.mod-plan-version; font-size: 16px; } BodyText { text: "Ephervex core checked compatibility with the selected instance."; wrap: word-wrap; } Rectangle { height: 116px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; VerticalLayout { padding: 12px; spacing: 5px; BodyText { text: root.mod-plan-install-count + " files will be installed"; color: Theme.text; } BodyText { text: root.mod-plan-dependency-count + " required dependencies"; } BodyText { text: root.mod-plan-existing-count + " matching managed mods are already installed"; } BodyText { text: root.mod-plan-optional-count + " optional dependencies are available but not selected"; } } } if root.mod-plan-warning-count > 0 : BodyText { text: "Warning: the plan has " + root.mod-plan-warning-count + " compatibility notes."; color: Theme.warning; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Cancel"; clicked => { root.modal = ""; } } AppButton { text: "Install Required Set"; tone: "primary"; enabled: root.mod-plan-install-count > 0; clicked => { root.install-mod(root.mod-plan-instance, root.mod-plan-project); root.modal = ""; } } } }
                    if root.modal == "eula" : VerticalLayout { spacing: 12px; BodyText { text: "Minecraft's server EULA is a legal agreement. Ephervex records global consent only after this explicit action; starting a server never accepts it for you."; wrap: word-wrap; } AppButton { text: "Open Minecraft EULA"; clicked => { root.open-eula-reference(); } } Rectangle { height: 70px; border-radius: 6px; border-width: 1px; border-color: root.eula-accepted ? Theme.success : Theme.warning; background: Theme.surface; BodyText { x: 12px; width: parent.width - 24px; text: root.eula-status; wrap: word-wrap; } } if !root.eula-accepted : BodyText { text: "By choosing Accept, you confirm that you have reviewed and agree to the Minecraft EULA."; color: Theme.text; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } if root.eula-accepted : AppButton { text: "Revoke Consent..."; tone: "danger"; clicked => { root.revoke-eula(); } } if !root.eula-accepted : AppButton { text: "Accept EULA"; tone: "primary"; clicked => { root.accept-eula(); } } } }
                    if root.modal == "kill" : VerticalLayout { spacing: 14px; BodyText { text: "Force killing \"" + (root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : "this server") + "\" may lose unsaved world data. Use normal Stop whenever the server responds."; color: Theme.warning; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Cancel"; clicked => { root.modal = ""; } } AppButton { text: "Force Kill"; tone: "danger"; clicked => { root.server-kill(root.selected-server-id); root.modal = ""; } } } }
                    if root.modal == "license" : ScrollView { Text { text: root.license-text; color: Theme.text-secondary; font-family: "monospace"; font-size: 12px; wrap: word-wrap; } }
                    if root.modal == "third-party" : VerticalLayout { spacing: 12px; BodyText { text: "An audited third-party license report is not bundled in this build yet. Ephervex does not invent dependency license information. The repository's Cargo metadata and lockfile remain the source inputs for a future generated license audit."; wrap: word-wrap; } BodyText { text: "Slint attribution is available directly in About & Licenses through Slint's official AboutSlint widget."; color: Theme.text-muted; wrap: word-wrap; } HorizontalLayout { Rectangle { horizontal-stretch: 1; } AppButton { text: "Close"; tone: "primary"; clicked => { root.modal = ""; } } } }
                }
            }
        }
    }
}
