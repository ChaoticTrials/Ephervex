slint::slint! {
    import { AboutSlint, ComboBox, ListView, ScrollView, Slider, Spinner } from "std-widgets.slint";

    export global GuiI18n {
        pure callback text(string) -> string;
        pure callback count(string, int) -> string;
        pure callback value(string, string) -> string;
        pure callback values(string, string, string) -> string;
    }

    export global Theme {
        in-out property <float> text-scale: 1;
        // The fixed-width family this machine actually has, resolved once at
        // startup. `font-family` is matched as a literal family name, so the
        // generic "monospace" written here matches nothing and only stands
        // until the real name is set. See the `monospace` module.
        in-out property <string> mono-family: "monospace";
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
        // The two mod catalogs, in their own brand colors, so a row says which
        // one a mod came from without being read.
        out property <color> modrinth: #1bd96a;
        out property <color> curseforge: #f16436;
        // The game's own grass, at the color a plains biome tints it.
        out property <color> vanilla: #91bd59;
        // The two loaders in their own marks' colors, the same way the two
        // catalogs above are. NeoForge is the rust its fox is drawn in. Forge's
        // anvil sits on a navy so dark (#1e2d44) that it disappears against
        // this background, so its own hue is kept and lifted into the range the
        // rest of this palette works in.
        out property <color> forge: #5981c0;
        out property <color> neoforge: #a44e37;
        // Every control that can share a toolbar row is this tall, so a row
        // that mixes a button, a chip and a field lines up without each caller
        // having to know what the others chose.
        out property <length> control-height: 36px;
        // The primary navigation column. Named because the page bodies beside
        // it have to know how much of the window is left.
        out property <length> sidebar-width: 218px;
        // One entity card's height and the gap under it. Named because the
        // cards and the arithmetic that scrolls one into view have to agree,
        // and a list of fixed-height rows is the only reason that arithmetic
        // can be written at all.
        out property <length> card-height: 112px;
        out property <length> card-gap: 12px;
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
        // Whether this launcher has a game running for the instance that it can
        // stop. Not the negation of `can-launch`: an instance that needs
        // attention cannot be launched and is not running either.
        can-kill: bool,
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
        can-connect: bool,
        connect-reason: string,
    }

    export struct ModRow {
        project-id: string,
        logical-path: string,
        title: string,
        file-name: string,
        version: string,
        version-known: bool,
        provider: string,
        // "modrinth", "curseforge", or empty for an entry with no catalog
        // identity. The display string above is what a reader sees; this is
        // what the row is colored by.
        provider-kind: string,
        origin: string,
        origin-kind: string,
        compatibility: string,
        author: string,
        icon: image,
        missing: bool,
        // Whether the loader will load this mod. A disabled mod is installed
        // and recorded; it is simply renamed out of the loader's way.
        enabled: bool,
        pack-owned: bool,
        can-manage: bool,
        can-change-version: bool,
        can-update: bool,
        can-remove: bool,
        restriction: string,
        // Why the newest crash report points at this mod, already localized.
        // Empty for a mod no crash implicates.
        suspect-reason: string,
        // "high", "medium", or "low". Stated evidence outranks inferred, and
        // the row is weighted accordingly.
        suspect-confidence: string,
    }

    export struct ModVersionRow {
        version-id: string,
        name: string,
        number: string,
        channel: string,
        published: string,
        installed: bool,
        newest: bool,
    }

    // One optional client component the import modal can offer. `logical-path`
    // is the stable identity a selection is toggled and submitted by;
    // `display` is what the row shows.
    export struct ImportOptionRow {
        logical-path: string,
        display: string,
        selected: bool,
    }

    // One file a provider refused to serve, still waiting on a watcher match
    // or a skip. `identity` is the opaque join key `skip-blocked-file`/
    // `open-blocked-file` are called with; the two lines are the project's
    // and the file's own names, falling back to their catalog ids.
    export struct ImportBlockedRow {
        identity: string,
        project-line: string,
        file-line: string,
        browser-url: string,
    }

    // One linked server in a mod confirmation, or one finished target in a
    // result. The same shape serves both so the two cannot describe the same
    // server differently.
    export struct LinkedTargetRow {
        id: string,
        name: string,
        detail: string,
        state: string,
        explanation: string,
        tone: string,
        actionable: bool,
        selected: bool,
    }

    export struct ModSearchRow {
        project-id: string,
        title: string,
        description: string,
        author: string,
        // The project's cached artwork, or an empty image when there is none.
        icon: image,
        // A short, already-localized note about the result, such as whether it
        // fits this instance. Not a number and not a catalog statistic.
        note: string,
        // Whether this instance already has the project. Decided against the
        // managed mods list, so it follows an install without a re-search.
        installed: bool,
        // Whether an install for this project is queued or under way.
        queued: bool,
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

    export struct RunningRow {
        // "client" or "server", which decides where every action goes.
        kind: string,
        id: string,
        // Which of that entity's launches this row is, empty for the first and
        // for every server.
        launch: string,
        name: string,
        pid: string,
        uptime: string,
        // Live CPU and memory. Empty on the first poll, because CPU is a rate
        // and there is nothing yet to compare against.
        usage: string,
        last-line: string,
        controllable: bool,
        can-stop: bool,
    }

    export struct LogChunk {
        text: string,
        level: string,
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

    // One product preference as the Minecraft defaults screen renders it.
    //
    // Everything a row needs and nothing it has to work out for itself: the
    // editor to use, the value to show, whether Ephervex applies it, and what
    // the previewed release does with it are all decided in Rust against the
    // bundled catalog and core's own compatibility answer.
    export struct MinecraftPreferenceRow {
        id: string,
        label: string,
        technical-id: string,
        // Non-empty on the first row of a group, so one flat list still reads
        // as sections.
        section-heading: string,
        editor: string,
        advanced: bool,
        overridden: bool,
        state-text: string,
        can-reset: bool,
        dirty: bool,
        bool-value: bool,
        // The numeric field's text, the window size's width, or the free-text
        // value, depending on the editor.
        text-value: string,
        height-text: string,
        number-value: float,
        number-min: float,
        number-max: float,
        number-step: float,
        number-readout: string,
        number-hint: string,
        // The window size picker's own presentation. Neither is stored.
        aspect-index: int,
        aspect-locked: bool,
        enum-labels: [string],
        enum-index: int,
        binding-label: string,
        invalid: bool,
        validation: string,
        notice: string,
        status: string,
        status-kind: string,
        status-detail: string,
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
        linked-client-id: string,
        // What the work is doing right now, and how far through it is. Empty
        // and indeterminate for a creation with nothing to report.
        detail: string,
        progress: float,
        indeterminate: bool,
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

    // A row of controls sitting side by side.
    //
    // A layout with no spacing puts two buttons edge to edge, which reads as one
    // control. Every such row goes through this so the gap cannot be forgotten
    // one row at a time.
    component ActionRow inherits HorizontalLayout {
        spacing: 8px;
    }

    component Divider inherits Rectangle {
        height: 1px;
        vertical-stretch: 0;
        background: Theme.border;
    }

    component AppButton inherits Rectangle {
        in property <string> text;
        // What assistive technology hears, when the visible label is too short
        // to be unambiguous on its own. Two "Browse..." buttons in one dialog
        // read identically otherwise.
        in property <string> accessible-name: root.text;
        in property <string> tone: "secondary";
        in property <bool> enabled: true;
        callback clicked();
        min-width: max(88px, label.preferred-width + 28px);
        height: Theme.control-height;
        horizontal-stretch: 0;
        border-radius: 6px;
        border-width: 1px;
        border-color: !enabled ? Theme.border : tone == "primary" ? Theme.accent : tone == "danger" ? Theme.danger : Theme.border;
        background: !enabled ? Theme.surface : area.pressed ? Theme.background : area.has-hover ? (tone == "primary" ? rgb(54, 139, 147) : tone == "danger" ? rgb(123, 48, 56) : Theme.elevated) : tone == "primary" ? rgb(45, 125, 132) : tone == "danger" ? rgb(97, 39, 46) : Theme.surface;
        accessible-role: button;
        accessible-label: root.accessible-name;
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

    // A star that says whether one Minecraft release is pinned, and toggles it.
    //
    // Colored rather than swapped for a second glyph when it is on: an outline
    // and a fill of the same shape read as one control in two states, where two
    // shapes read as two controls.
    component FavoriteToggle inherits Rectangle {
        in property <bool> favorite;
        in property <string> version;
        in property <bool> enabled: true;
        callback toggled();
        width: Theme.control-height;
        height: Theme.control-height;
        border-radius: 6px;
        border-width: 1px;
        border-color: !root.enabled ? Theme.border : root.favorite ? Theme.warning.with-alpha(0.7) : area.has-hover ? Theme.accent : Theme.border;
        background: area.has-hover && root.enabled ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: root.favorite ? GuiI18n.value("gui-unfavorite-version", root.version) : GuiI18n.value("gui-favorite-version", root.version);
        accessible-enabled: root.enabled;
        accessible-checkable: true;
        accessible-checked: root.favorite;
        accessible-action-default => { if root.enabled { root.toggled(); } }
        Image {
            x: 8px; y: 8px; width: parent.width - 16px; height: parent.height - 16px;
            source: root.favorite ? @image-url("../../../assets/icons/favorite-filled.svg") : @image-url("../../../assets/icons/favorite.svg");
            image-fit: contain;
            colorize: !root.enabled ? Theme.text-muted : root.favorite ? Theme.warning : Theme.text-secondary;
        }
        area := TouchArea { enabled: root.enabled; mouse-cursor: pointer; clicked => { root.toggled(); } }
    }

    component IconButton inherits Rectangle {
        in property <image> icon;
        in property <string> accessible-name;
        in property <bool> enabled: true;
        callback clicked();
        width: Theme.control-height;
        height: Theme.control-height;
        border-radius: 6px;
        border-width: 1px;
        border-color: area.has-hover ? Theme.accent : Theme.border;
        background: area.has-hover ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: accessible-name;
        accessible-enabled: enabled;
        accessible-action-default => { if root.enabled { root.clicked(); } }
        // A disabled button keeps its border and loses its click, so the glyph
        // has to go quiet too or the control still reads as available.
        Image { x: 8px; y: 8px; width: parent.width - 16px; height: parent.height - 16px; source: root.icon; image-fit: contain; colorize: root.enabled ? Theme.text-secondary : Theme.text-muted; }
        area := TouchArea { enabled: root.enabled; clicked => { root.clicked(); } }
    }

    component TextField inherits Rectangle {
        in-out property <string> text <=> input.text;
        in property <string> placeholder;
        in property <string> accessible-name: placeholder;
        in property <bool> enabled: true;
        in property <bool> invalid: false;
        in property <InputType> input-type: InputType.text;
        callback accepted();
        callback edited(string);
        callback focus-lost();
        height: Theme.control-height;
        min-width: 120px;
        border-radius: 6px;
        border-width: 1px;
        border-color: invalid ? Theme.danger : input.has-focus ? Theme.accent : Theme.border;
        background: Theme.background;
        accessible-role: text-input;
        accessible-label: accessible-name;
        accessible-enabled: enabled;
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
            // A field whose value is part of a larger draft has to be able to
            // say "I am done" without the user pressing Enter.
            changed has-focus => { if (!self.has-focus) { root.focus-lost(); } }
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
        in property <string> font-family: Theme.mono-family;
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

    component FilterChip inherits Rectangle {
        in property <string> text;
        in property <string> accessible-name: text;
        in property <bool> selected;
        callback clicked();
        height: Theme.control-height;
        width: label.preferred-width + 24px;
        horizontal-stretch: 0;
        border-radius: 5px;
        border-width: 1px;
        border-color: selected ? Theme.accent : Theme.border;
        background: selected ? #244a4e : area.has-hover ? Theme.elevated : transparent;
        accessible-role: checkbox;
        accessible-label: accessible-name;
        accessible-checked: selected;
        accessible-action-default => { root.clicked(); }
        label := Text {
            text: root.text;
            color: root.selected ? Theme.accent : Theme.text-secondary;
            font-size: Theme.caption-size;
            font-weight: root.selected ? 600 : 400;
            horizontal-alignment: center;
            vertical-alignment: center;
        }
        area := TouchArea { clicked => { root.clicked(); } }
    }

    component LiveLogViewer inherits Rectangle {
        in property <[LogChunk]> chunks;
        in property <bool> has-content;
        in property <string> status;
        in property <bool> history-truncated;
        in property <int> pending-lines;
        in property <int> content-columns: 80;
        in property <length> font-size: 12px;
        in-out property <string> query;
        in-out property <bool> wrap-lines;
        in-out property <bool> follow;
        in property <bool> trace-enabled;
        in property <bool> debug-enabled;
        in property <bool> info-enabled;
        in property <bool> warn-enabled;
        in property <bool> error-enabled;
        in property <bool> fatal-enabled;
        in property <bool> other-enabled;
        in property <int> revision;
        // The selection, in reading order, as byte offsets into each end's own
        // row. -1 means nothing is selected. Offsets rather than columns
        // because that is what a text item takes, and a column is only a byte
        // offset while the line stays ASCII.
        in property <int> selection-start-row: -1;
        in property <int> selection-start-offset;
        in property <int> selection-end-row: -1;
        in property <int> selection-end-offset;
        in property <bool> has-selection;
        in property <int> selection-revision;
        callback set-level(string, bool);
        callback search(string);
        callback copy();
        callback copy-selection();
        callback select-at(int, int, bool);
        callback select-word-at(int, int);
        callback select-all();
        callback follow-changed(bool);
        // Ten characters wide divided by ten. Exact in a fixed-width family,
        // and the reason the monospace family has to resolve before a column
        // can be worked out from a pointer position.
        private property <length> advance: probe.preferred-width / 10;
        // Every row is one physical line, so they are all this tall, which is
        // what lets the row under the pointer be counted rather than searched
        // for.
        private property <length> row-height: probe.preferred-height + 8px;
        border-radius: 8px;
        border-width: 1px;
        border-color: Theme.border;
        background: rgb(11, 14, 16);

        // Measured, never drawn. A hidden text still reports the size it would
        // take, which is the only way to learn a font's advance, and a
        // fixed-width family makes that one number true of every character.
        probe := Text { visible: false; width: 0px; height: 0px; text: "0000000000"; font-family: Theme.mono-family; font-size: root.font-size; }

        VerticalLayout {
            spacing: 0px;
            Rectangle {
                height: Theme.control-height + 12px;
                background: Theme.surface;
                HorizontalLayout {
                    padding-left: 10px; padding-right: 10px; spacing: 6px; cross-axis-alignment: center;
                    all-levels := FilterChip { text: GuiI18n.text("gui-all"); accessible-name: GuiI18n.text("gui-filter-all-levels"); selected: root.trace-enabled && root.debug-enabled && root.info-enabled && root.warn-enabled && root.error-enabled && root.fatal-enabled && root.other-enabled; clicked => { root.set-level("all", !all-levels.selected); } }
                    FilterChip { text: "INFO"; accessible-name: GuiI18n.text("gui-filter-info"); selected: root.info-enabled; clicked => { root.set-level("info", !root.info-enabled); } }
                    FilterChip { text: "WARN"; accessible-name: GuiI18n.text("gui-filter-warn"); selected: root.warn-enabled; clicked => { root.set-level("warn", !root.warn-enabled); } }
                    FilterChip { text: "ERROR"; accessible-name: GuiI18n.text("gui-filter-error"); selected: root.error-enabled; clicked => { root.set-level("error", !root.error-enabled); } }
                    FilterChip { text: "DEBUG"; accessible-name: GuiI18n.text("gui-filter-debug"); selected: root.debug-enabled; clicked => { root.set-level("debug", !root.debug-enabled); } }
                    FilterChip { text: "TRACE"; accessible-name: GuiI18n.text("gui-filter-trace"); selected: root.trace-enabled; clicked => { root.set-level("trace", !root.trace-enabled); } }
                    FilterChip { text: "FATAL"; accessible-name: GuiI18n.text("gui-filter-fatal"); selected: root.fatal-enabled; clicked => { root.set-level("fatal", !root.fatal-enabled); } }
                    FilterChip { text: GuiI18n.text("gui-log-level-other"); accessible-name: GuiI18n.text("gui-filter-other"); selected: root.other-enabled; clicked => { root.set-level("other", !root.other-enabled); } }
                    Rectangle { horizontal-stretch: 1; }
                }
            }
            Rectangle {
                height: Theme.control-height + 12px;
                background: Theme.surface;
                HorizontalLayout {
                    padding-left: 10px; padding-right: 10px; spacing: 8px; cross-axis-alignment: center;
                    TextField { text <=> root.query; placeholder: GuiI18n.text("gui-search-retained-logs"); input-type: InputType.search; width: 230px; accepted => { root.search(root.query); } accessible-label: GuiI18n.text("gui-search-retained-logs"); }
                    AppButton { text: GuiI18n.text("gui-find"); min-width: 62px; clicked => { root.search(root.query); } }
                    FilterChip { text: GuiI18n.text("gui-wrap-lines"); accessible-name: GuiI18n.text("gui-wrap-lines"); selected: root.wrap-lines; clicked => { root.wrap-lines = !root.wrap-lines; } }
                    FilterChip { text: root.follow ? GuiI18n.text("gui-following") : GuiI18n.text("gui-jump-latest"); accessible-name: root.follow ? GuiI18n.text("gui-pause-following") : GuiI18n.text("gui-jump-latest"); selected: root.follow; clicked => { root.follow = !root.follow; root.follow-changed(root.follow); } }
                    AppButton { text: GuiI18n.text("gui-copy-selection"); min-width: 104px; enabled: root.has-selection; clicked => { root.copy-selection(); } }
                    AppButton { text: GuiI18n.text("gui-copy-retained"); min-width: 104px; enabled: root.has-content; clicked => { root.copy(); } }
                    Rectangle { horizontal-stretch: 1; }
                    if root.pending-lines > 0 : BodyText { text: GuiI18n.count("gui-new-log-lines", root.pending-lines); color: Theme.accent; font-size: Theme.caption-size; }
                }
            }
            if root.status != "" || root.history-truncated : Rectangle {
                height: 28px;
                background: Theme.background;
                HorizontalLayout {
                    padding-left: 10px; padding-right: 10px; spacing: 12px;
                    BodyText { text: root.status; color: Theme.text-muted; font-size: Theme.caption-size; }
                    if root.history-truncated : BodyText { text: GuiI18n.text("gui-retained-history-truncated"); color: Theme.warning; font-size: Theme.caption-size; }
                    Rectangle { horizontal-stretch: 1; }
                }
            }
            if root.chunks.length == 0 : Rectangle { background: rgb(11, 14, 16); VerticalLayout { alignment: center; spacing: 6px; Text { text: GuiI18n.text("gui-no-log-output"); color: Theme.text; font-size: Theme.heading-size; horizontal-alignment: center; } BodyText { text: GuiI18n.text("gui-no-log-output-detail"); horizontal-alignment: center; } } }
            if root.chunks.length > 0 : Rectangle {
              keys := FocusScope {
                key-pressed(event) => {
                    if event.modifiers.control && event.text == "c" {
                        root.copy-selection();
                        return accept;
                    }
                    if event.modifiers.control && event.text == "a" {
                        root.select-all();
                        return accept;
                    }
                    return reject;
                }
              }
              output := ListView {
                // Measured from the font rather than estimated from its size:
                // the widest line has to be reachable exactly, not
                // approximately.
                viewport-width: root.wrap-lines ? self.visible-width : max(self.visible-width, root.content-columns * root.advance + 24px);
                // A bar the user can find. Wrapped text has nowhere to go
                // sideways, so the bar is gone rather than idle.
                horizontal-scrollbar-policy: root.wrap-lines ? ScrollBarPolicy.always-off : ScrollBarPolicy.always-on;
                private property <int> observed-revision: root.revision;
                changed observed-revision => {
                    if root.follow {
                        self.viewport-y = min(0px, self.visible-height - self.viewport-height);
                    }
                }
                for chunk[row] in root.chunks : Rectangle {
                    width: output.viewport-width;
                    // One line tall, except when wrapping turns one line into
                    // several: a row shorter than its own text overlaps the row
                    // under it.
                    height: root.wrap-lines ? chunk-input.preferred-height + 8px : root.row-height;
                    background: transparent;
                    accessible-role: text-input;
                    accessible-read-only: true;
                    accessible-value: chunk.text;
                    // The whole line when this row is inside the selection,
                    // and the part on the right side of the boundary when it
                    // is the first of it. An offset past the end of the text
                    // is clamped by the text item, so one very large number
                    // stands for "to the end of this line".
                    private property <int> selection-from: row == root.selection-start-row ? root.selection-start-offset : 0;
                    private property <int> selection-to: row == root.selection-end-row ? root.selection-end-offset : 2147483647;
                    private property <bool> selected: root.selection-start-row >= 0 && row >= root.selection-start-row && row <= root.selection-end-row;
                    // A selection is applied rather than bound, because a text
                    // item takes it as a call. Applied on init as well as on
                    // change, because the list is virtualized: a row scrolled
                    // into view is built after the selection was made and would
                    // otherwise come back empty-handed.
                    function apply-selection() {
                        if self.selected {
                            chunk-input.set-selection-offsets(self.selection-from, self.selection-to);
                        } else {
                            chunk-input.set-selection-offsets(0, 0);
                        }
                    }
                    private property <int> observed-selection: root.selection-revision;
                    changed observed-selection => { self.apply-selection(); }
                    init => { self.apply-selection(); }
                    ContextMenuArea {
                        Menu {
                            MenuItem { title: GuiI18n.text("gui-copy-selection"); enabled: root.has-selection; activated => { root.copy-selection(); } }
                            MenuItem { title: GuiI18n.text("gui-select-all"); enabled: root.chunks.length > 0; activated => { root.select-all(); } }
                        }
                        chunk-input := TextInput {
                            x: 10px; y: 4px;
                            width: root.wrap-lines ? output.visible-width - 20px : max(output.visible-width - 20px, self.preferred-width);
                            height: self.preferred-height;
                            text: chunk.text;
                            color: chunk.level == "error" || chunk.level == "fatal" ? #ef9a9f : chunk.level == "warn" ? Theme.warning : chunk.level == "debug" || chunk.level == "trace" ? Theme.text-muted : Theme.text-secondary;
                            selection-background-color: Theme.accent;
                            selection-foreground-color: Theme.background;
                            read-only: true;
                            single-line: false;
                            wrap: root.wrap-lines ? word-wrap : no-wrap;
                            font-family: Theme.mono-family;
                            font-size: root.font-size;
                            accessible-label: GuiI18n.text("gui-log-output-block");
                        }
                    }
                    // Declared last, so it takes the pointer before the text
                    // item can start a selection of its own. A text item's
                    // selection cannot leave the element it belongs to, which
                    // is the whole reason a drag never used to reach the next
                    // line.
                    //
                    // With every row one line of the same height, the row under
                    // the pointer is this row plus a count of row heights. No
                    // geometry is queried and no absolute position is needed,
                    // which is what keeps this correct inside a virtualized
                    // list.
                    //
                    // Wrapped lines are the documented exception: they are not
                    // uniform in height, so a drag then selects within its own
                    // row. Shift-clicking still reaches across rows, because
                    // that lands on a real row's own area and needs no
                    // arithmetic.
                    select := TouchArea {
                        mouse-cursor: text;
                        pointer-event(event) => {
                            if event.kind == PointerEventKind.down {
                                keys.focus();
                                root.select-at(row, self.column-at(self.mouse-x), event.modifiers.shift);
                            }
                        }
                        // Runs after the press that opened it, so it replaces
                        // the caret that press placed rather than fighting it.
                        double-clicked => {
                            root.select-word-at(row, self.column-at(self.mouse-x));
                        }
                        moved => {
                            if self.pressed {
                                root.select-at(self.row-at(self.mouse-y), self.column-at(self.mouse-x), true);
                                // Dragging past an edge keeps going. Stepped by
                                // one row per movement rather than animated, so
                                // the selection and the view stay in step.
                                if self.absolute-position.y + self.mouse-y < output.absolute-position.y {
                                    output.viewport-y = min(0px, output.viewport-y + root.row-height);
                                } else if self.absolute-position.y + self.mouse-y > output.absolute-position.y + output.visible-height {
                                    output.viewport-y = max(min(0px, output.visible-height - output.viewport-height), output.viewport-y - root.row-height);
                                }
                            }
                        }
                        // Rounded, not truncated: a caret sits between two
                        // characters, so the column the pointer means is the
                        // nearest gap rather than the one it has fully passed.
                        // Truncating puts the boundary a whole character behind
                        // the pointer, which reads as the wrong character being
                        // highlighted.
                        function column-at(x: length) -> int {
                            return Math.max(0, Math.round((x - 10px) / root.advance));
                        }
                        function row-at(y: length) -> int {
                            if root.wrap-lines {
                                return row;
                            }
                            return Math.clamp(row + Math.floor(y / root.row-height), 0, root.chunks.length - 1);
                        }
                    }
                }
              }
              // The bottom is where a log is read from, and scrolling up is how
              // it is left. Following only moves the view when the next line
              // arrives, which is no help at all on a log that has stopped, so
              // this moves it now and resumes following in one press.
              //
              // Shown only when there is somewhere to go: a view already at the
              // end would offer a button that does nothing.
              if output.viewport-y > output.visible-height - output.viewport-height + 2px : IconButton {
                  x: parent.width - self.width - 18px;
                  y: parent.height - self.height - 14px;
                  icon: @image-url("../../../assets/icons/jump-to-end.svg");
                  accessible-name: GuiI18n.text("gui-jump-latest");
                  background: Theme.elevated;
                  clicked => {
                      output.viewport-y = min(0px, output.visible-height - output.viewport-height);
                      if !root.follow {
                          root.follow = true;
                          root.follow-changed(true);
                      }
                  }
              }
            }
        }
    }

    // The list of files one diagnostics section holds.
    //
    // One component because logs and crash reports are listed the same way on
    // both workspaces, and four copies of a list drift.
    component DiagnosticFileList inherits Rectangle {
        in property <[DiagnosticRow]> items;
        in property <string> selected;
        // Crash reports are named in the warning color wherever they appear,
        // because a crash report existing is itself the finding.
        in property <bool> crash;
        callback opened(string);
        width: 270px;
        background: Theme.surface;
        border-radius: 8px;
        border-width: 1px;
        border-color: Theme.border;
        VerticalLayout {
            padding: 8px;
            spacing: 5px;
            BodyText { text: GuiI18n.count("gui-diagnostic-files-count", root.items.length); }
            ScrollView {
                VerticalLayout {
                    spacing: 5px;
                    for item in root.items : Rectangle {
                        height: 66px;
                        background: open-area.has-hover ? Theme.elevated : Theme.background;
                        border-radius: 6px;
                        border-width: item.name == root.selected ? 2px : 1px;
                        border-color: item.name == root.selected ? Theme.accent : open-area.has-hover ? Theme.accent.with-alpha(0.7) : Theme.border;
                        accessible-role: button;
                        accessible-label: item.name;
                        accessible-action-default => { root.opened(item.name); }
                        VerticalLayout {
                            padding: 8px;
                            Text { text: item.name; color: root.crash ? Theme.warning : Theme.text; overflow: elide; }
                            BodyText { text: item.summary != "" ? item.summary : item.size; overflow: elide; font-size: Theme.caption-size; }
                        }
                        // Hover is the whole affordance here: without it a row
                        // that opens on one press looks like a label.
                        open-area := TouchArea { mouse-cursor: pointer; clicked => { root.opened(item.name); } }
                    }
                }
            }
        }
    }

    component Toggle inherits Rectangle {
        in-out property <bool> checked;
        in property <bool> enabled: true;
        in property <string> accessible-name;
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
        accessible-label: accessible-name;
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
            Toggle { accessible-name: root.label; checked <=> root.checked; enabled: root.enabled; toggled(value) => { root.toggled(value); } }
        }
    }

    // Launching as somebody other than the default account.
    //
    // Beside Launch rather than replacing it: the common case is one account,
    // and a launcher that made everyone choose every time would be worse for
    // it. Choosing here applies to one launch and never changes the default,
    // which is what makes it safe to use for a quick second client.
    component LaunchAsButton inherits Rectangle {
        in property <[AccountRow]> accounts;
        in property <bool> enabled: true;
        // Whether the instance is already running, so a launch from here is a
        // second client rather than the first.
        in property <bool> alongside;
        // The account, and whether this is a second client alongside another.
        callback launch-as(string, bool);
        callback add-account();
        // Sized the way `AppButton` is, so a row that pairs the two splits
        // evenly instead of letting the other one absorb the slack.
        min-width: max(88px, content.preferred-width);
        height: Theme.control-height;
        horizontal-stretch: 0;
        border-radius: 6px;
        border-width: 1px;
        border-color: area.has-hover && root.enabled ? Theme.accent : Theme.border;
        background: area.has-hover && root.enabled ? Theme.elevated : transparent;
        accessible-role: button;
        accessible-label: GuiI18n.text("gui-launch-as");
        accessible-enabled: enabled;
        accessible-action-default => { if root.enabled { menu.show({ x: 0, y: root.height }); } }
        menu := ContextMenuArea {
            Menu {
                for account in root.accounts : MenuItem {
                    // The default is named like the rest. It is still the one
                    // plain Launch uses, and saying so twice would read as two
                    // different accounts.
                    title: account.is-default ? account.name + " (" + GuiI18n.text("gui-default") + ")" : account.name;
                    activated => { root.launch-as(account.id, root.alongside); }
                }
                MenuSeparator {}
                MenuItem {
                    title: GuiI18n.text("gui-add-account");
                    activated => { root.add-account(); }
                }
            }
        }
        content := HorizontalLayout {
            padding-left: 14px; padding-right: 14px; spacing: 5px; alignment: center;
            Text {
                text: GuiI18n.text("gui-launch-as");
                color: root.enabled ? Theme.text : Theme.text-muted;
                font-size: Theme.label-size;
                vertical-alignment: center;
            }
            Text {
                text: "\u{25be}";
                color: root.enabled ? Theme.text-secondary : Theme.text-muted;
                font-size: Theme.label-size;
                vertical-alignment: center;
            }
        }
        // Declared after the menu area, so a press opens the menu rather than
        // being swallowed by the area that hosts it.
        area := TouchArea {
            enabled: root.enabled;
            mouse-cursor: pointer;
            clicked => { menu.show({ x: 0, y: root.height }); }
        }
    }

    component Pill inherits Rectangle {
        in property <string> text;
        in property <color> tone-color: Theme.text-secondary;
        // A pill normally hugs its label. Where one stands in for a control it
        // takes that control's size instead, so a column of rows does not jump
        // between the two. An explicit width on the instance cannot do this:
        // the width here is a binding on the same element and a laid-out child
        // with a bound width is fixed at it.
        in property <length> min-content-width: 0px;
        in property <length> content-height: 24px;
        height: root.content-height;
        width: max(root.min-content-width, label.preferred-width + 18px);
        horizontal-stretch: 0;
        accessible-role: AccessibleRole.text;
        accessible-label: root.text;
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

    component LinkedTargetsPanel inherits Rectangle {
        in property <[LinkedTargetRow]> rows;
        // Result lists are read-only; a confirmation's are not.
        in property <bool> selectable: false;
        callback toggled(int);
        border-radius: 6px;
        border-width: 1px;
        border-color: Theme.border;
        background: Theme.background;
        ScrollView {
            viewport-width: self.width;
            VerticalLayout { alignment: start; padding: 8px; spacing: 6px;
                for row[index] in root.rows : Rectangle {
                    height: 62px;
                    border-radius: 5px;
                    border-width: row.selected ? 2px : 1px;
                    border-color: row.selected ? Theme.accent : Theme.border.with-alpha(0.6);
                    background: row.actionable || !root.selectable ? Theme.surface : Theme.surface.with-alpha(0.45);
                    HorizontalLayout { padding-left: 10px; padding-right: 10px; spacing: 10px;
                        // The explanation gets the full row width: a refusal is a
                        // sentence, and eliding it to a column loses the reason.
                        VerticalLayout { alignment: center; spacing: 1px; horizontal-stretch: 1;
                            HorizontalLayout { spacing: 8px;
                                Text { text: row.name; color: row.actionable || !root.selectable ? Theme.text : Theme.text-secondary; font-size: Theme.label-size; font-weight: 600; overflow: elide; horizontal-stretch: 1; }
                                Text { text: row.detail; color: Theme.text-muted; font-size: Theme.caption-size; vertical-alignment: center; overflow: elide; horizontal-stretch: 0; }
                            }
                            Text { text: row.explanation; color: Theme.text-muted; font-size: Theme.caption-size; overflow: elide; }
                        }
                        VerticalLayout { alignment: center; max-width: 150px;
                            Text { text: row.state; color: row.tone == "ok" ? Theme.success : row.tone == "warn" ? Theme.warning : row.tone == "error" ? Theme.danger : Theme.text-secondary; font-size: Theme.label-size; horizontal-alignment: right; overflow: elide; }
                        }
                    }
                    TouchArea { enabled: root.selectable && row.actionable; mouse-cursor: pointer; clicked => { root.toggled(index); } }
                }
            }
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
            // `alignment` is the main axis only. Without the cross-axis rule a
            // fixed-size image is pinned to the top of the row while the label,
            // which has no fixed height, stretches and centers itself inside it.
            cross-axis-alignment: center;
            padding-left: 16px; padding-right: 12px; spacing: 13px;
            // Colorized here rather than in the file, so the glyph follows the
            // selected state along with the label and the marker beside it.
            Image { source: root.icon; width: 22px; height: 22px; image-fit: contain; colorize: root.selected ? Theme.accent : Theme.text-secondary; }
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
        // Every glyph is drawn in the same 48-unit box at the same stroke
        // weight, so one geometry serves all of them and a server does not come
        // out visibly heavier than the loader beside it in the same list.
        //
        // Colorized from the tile's own border, so the glyph and the frame
        // around it can never disagree and the tones live in `Theme` alone.
        Image {
            x: 7px; y: 7px; width: 38px; height: 38px;
            source: root.server ? @image-url("../../../assets/icons/environment-server.svg") : root.kind == "forge" ? @image-url("../../../assets/icons/environment-forge.svg") : root.kind == "neoforge" ? @image-url("../../../assets/icons/environment-neoforge.svg") : @image-url("../../../assets/icons/environment-vanilla.svg");
            image-fit: contain;
            colorize: root.border-color;
        }
    }

    // A mod's artwork, or the same shape without it. The rectangle is drawn
    // whether or not an icon arrived, so a list whose images are still loading
    // — or that will never get any — has the same layout as one whose images
    // are all present.
    component ModArtwork inherits Rectangle {
        in property <image> icon;
        in property <string> origin-kind;
        // The tile's edge length. Everything inside is derived from it so a
        // caller changes one number rather than three.
        in property <length> size: 56px;
        property <color> tone: root.origin-kind == "pack" ? Theme.accent-secondary : root.origin-kind == "dependency" ? Theme.text-muted : Theme.accent;
        width: root.size; height: root.size;
        border-radius: root.size / 7;
        border-width: 1px;
        border-color: root.tone.with-alpha(0.45);
        background: root.tone.with-alpha(0.1);
        clip: true;
        if root.icon.width > 0 : Image { x: 3px; y: 3px; width: root.size - 6px; height: root.size - 6px; source: root.icon; image-fit: contain; }
        if root.icon.width == 0 : Image { x: root.size / 4; y: root.size / 4; width: root.size / 2; height: root.size / 2; source: @image-url("../../../assets/icons/semantic-mod.svg"); image-fit: contain; colorize: root.tone; }
    }

    // One row of the Managed Mods list.
    //
    // Two lines of text and one badge. The provenance badge is the only badge:
    // everything else that distinguishes a row — an unknown version, a missing
    // file, the artwork's tone — is said in the place it belongs rather than
    // added as another chip.
    component ModCard inherits Rectangle {
        in property <ModRow> item;
        // Whether this list lets a mod be turned off in place.
        in property <bool> togglable;
        callback manage(string);
        callback toggle(string, bool);
        height: 86px;
        border-radius: 6px;
        border-width: 1px;
        border-color: root.item.missing ? Theme.warning.with-alpha(0.6) : root.item.suspect-confidence == "high" ? Theme.danger.with-alpha(0.7) : root.item.suspect-confidence != "" ? Theme.warning.with-alpha(0.6) : touch.has-hover && root.item.can-manage ? Theme.accent.with-alpha(0.7) : Theme.border;
        background: Theme.background;
        touch := TouchArea {
            enabled: root.item.can-manage;
            mouse-cursor: pointer;
            clicked => { root.manage(root.item.logical-path); }
        }
        HorizontalLayout {
            padding: 10px; spacing: 11px;
            VerticalLayout { alignment: center; ModArtwork { icon: root.item.icon; origin-kind: root.item.origin-kind; } }
            VerticalLayout {
                alignment: center; spacing: 3px; horizontal-stretch: 1;
                HorizontalLayout {
                    spacing: 8px;
                    Text {
                        text: root.item.title;
                        color: root.item.enabled ? Theme.text : Theme.text-muted;
                        font-size: Theme.row-heading-size;
                        font-weight: 600;
                        overflow: elide;
                        horizontal-stretch: 1;
                        vertical-alignment: center;
                    }
                    VerticalLayout { alignment: center; Pill { text: root.item.origin; tone-color: root.item.origin-kind == "pack" ? Theme.accent-secondary : root.item.origin-kind == "dependency" ? Theme.text-muted : Theme.accent; } }
                    // Which catalog the mod came from, in that catalog's own
                    // color. Absent for an entry with no catalog identity,
                    // where the version line already says so.
                    if root.item.provider-kind != "" : VerticalLayout { alignment: center; Pill { text: root.item.provider; tone-color: root.item.provider-kind == "curseforge" ? Theme.curseforge : Theme.modrinth; } }
                    // Declared after the card's own touch area, so it takes its
                    // own clicks rather than opening the mod manager.
                    if root.togglable : VerticalLayout { alignment: center; Toggle { checked: root.item.enabled; enabled: !root.item.missing; accessible-name: GuiI18n.value("gui-mod-loads", root.item.title); toggled(on) => { root.toggle(root.item.logical-path, on); } } }
                    VerticalLayout { alignment: center; Text { text: root.item.can-manage ? "›" : ""; color: touch.has-hover ? Theme.accent : Theme.text-muted; font-size: Theme.section-size; width: 10px; } }
                }
                Text {
                    text: root.item.provider-kind == "" ? root.item.version + " | " + root.item.provider : root.item.version;
                    color: root.item.version-known ? Theme.text-secondary : Theme.text-muted;
                    font-size: Theme.label-size;
                    overflow: elide;
                }
                Text {
                    // What the crash says outranks the descriptive line: a
                    // reader looking at a crashed server is looking for this.
                    text: root.item.suspect-reason != "" ? root.item.suspect-reason : root.item.missing ? GuiI18n.text("gui-mod-file-missing") : !root.item.enabled ? GuiI18n.text("gui-mod-disabled") : root.item.compatibility != "" ? root.item.compatibility : root.item.file-name;
                    color: root.item.suspect-confidence == "high" ? Theme.danger : root.item.suspect-confidence != "" ? Theme.warning : root.item.missing ? Theme.warning : Theme.text-muted;
                    font-size: Theme.caption-size;
                    overflow: elide;
                }
            }
        }
    }

    // A layout at the root rather than a rectangle holding one.
    //
    // A layout fills the cross axis of the layout it sits in; a plain rectangle
    // takes the size of its own content. This is a tab body and a panel body,
    // so it has to fill: as a rectangle it turned a whole tab into a narrow
    // column of centred lines while every other tab, all of them layouts,
    // spanned the window.
    component EmptyState inherits VerticalLayout {
        in property <string> title;
        in property <string> message;
        // What is missing, said in a glyph as well as in the heading. The
        // neutral environment mark stands for a list this application has
        // nothing better to show, so a caller only names an icon when the
        // absent thing has one of its own.
        in property <image> icon: @image-url("../../../assets/icons/environment-generic.svg");
        callback primary();
        callback secondary();
        in property <string> primary-text;
        in property <string> secondary-text;
        alignment: center;
        spacing: 10px;
        padding: 32px;
        // Centred as a block and kept to a readable measure, rather than one
        // sentence stretched across a whole window.
        HorizontalLayout {
            alignment: center;
            VerticalLayout {
            max-width: 560px;
            alignment: center; spacing: 10px;
            HorizontalLayout { alignment: center; Image { source: root.icon; width: 64px; height: 64px; image-fit: contain; colorize: Theme.text-muted; } }
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
    }

    // What one exact Minecraft release does with one row.
    //
    // The badge says it in words rather than in color alone, and carries the
    // full sentence as its accessible label so a screen reader gets the
    // reason, not just the verdict.
    // What the newest crash report says, above the list it points into.
    //
    // Two placements, one component: on a tab that is not the mods list it
    // offers to open that list, and on the mods list itself it offers to act on
    // it. The suggestion count is what decides whether acting is even possible;
    // a report naming only mods that are already disabled offers nothing.
    component CrashNotice inherits Rectangle {
        in property <string> report;
        in property <string> failure;
        in property <bool> client-code-on-server;
        in property <int> suggested;
        // Whether this sits on the panel whose rows carry the suspect marks.
        in property <bool> beside-the-mods;
        callback review();
        callback disable-suggested();
        height: content.preferred-height;
        border-radius: 6px;
        border-width: 1px;
        border-color: Theme.danger.with-alpha(0.6);
        background: Theme.danger.with-alpha(0.08);
        content := HorizontalLayout {
            padding: 10px;
            spacing: 10px;
            VerticalLayout {
                alignment: center;
                spacing: 3px;
                horizontal-stretch: 1;
                Text {
                    text: GuiI18n.text("gui-crash-title");
                    color: Theme.danger;
                    font-size: Theme.row-heading-size;
                    font-weight: 600;
                    overflow: elide;
                }
                // Exact, because the thrown type and its message are what a bug
                // report has to quote.
                BodyText {
                    text: root.failure != "" ? root.failure : root.report;
                    color: Theme.text-secondary;
                    font-size: Theme.caption-size;
                    wrap: word-wrap;
                }
                if root.client-code-on-server : BodyText {
                    text: GuiI18n.text("crash-client-code-on-server");
                    color: Theme.text-secondary;
                    font-size: Theme.caption-size;
                    wrap: word-wrap;
                }
            }
            VerticalLayout {
                alignment: center;
                if !root.beside-the-mods : AppButton {
                    text: GuiI18n.text("gui-crash-review-mods");
                    tone: "primary";
                    clicked => { root.review(); }
                }
                if root.beside-the-mods : AppButton {
                    text: GuiI18n.count("gui-crash-disable-suggested", root.suggested);
                    tone: "primary";
                    enabled: root.suggested > 0;
                    clicked => { root.disable-suggested(); }
                }
            }
        }
    }

    component CompatibilityBadge inherits Rectangle {
        in property <string> text;
        in property <string> kind;
        in property <string> detail;
        property <color> tone: root.kind == "supported" ? Theme.success
            : root.kind == "unavailable" ? Theme.text-muted
            : root.kind == "unrepresentable" ? Theme.danger
            : Theme.warning;
        visible: root.text != "";
        width: root.text == "" ? 0px : label.preferred-width + 18px;
        height: 24px;
        horizontal-stretch: 0;
        border-radius: 5px;
        border-width: 1px;
        border-color: root.tone.with-alpha(0.55);
        background: root.tone.with-alpha(0.12);
        accessible-role: AccessibleRole.text;
        accessible-label: root.detail == "" ? root.text : root.text + ". " + root.detail;
        label := Text {
            text: root.text;
            color: root.tone;
            font-size: Theme.caption-size;
            font-weight: 600;
            horizontal-alignment: center;
            vertical-alignment: center;
        }
    }

    // A key binding, captured from this control's own focused input.
    //
    // Nothing here listens outside the window: the control takes focus, reads
    // the next key or mouse press delivered to it, and hands the raw report to
    // Rust, which decides whether Minecraft can name it. Escape leaves the
    // binding alone, Delete and Backspace unbind it, and losing focus is the
    // same as cancelling.
    component PreferenceKeyBindingEditor inherits Rectangle {
        in property <string> text;
        in property <bool> capturing;
        in property <string> accessible-name;
        callback begin();
        callback cancel();
        // The pressed text, exactly as the toolkit reported it. What that
        // identifies, and whether it identifies anything at all, is decided
        // in Rust.
        callback captured-key(string);
        callback captured-mouse(string);
        height: 34px;
        // Wide enough for the capture prompt and for the longest binding name
        // a mouse button produces, so neither is elided into a guess.
        min-width: 210px;
        horizontal-stretch: 0;
        border-radius: 6px;
        border-width: root.capturing ? 2px : 1px;
        border-color: root.capturing ? Theme.accent : scope.has-focus ? Theme.accent : Theme.border;
        background: root.capturing ? rgb(22, 56, 62) : Theme.background;
        accessible-role: button;
        accessible-label: root.accessible-name;
        accessible-value: root.text;
        accessible-action-default => { root.begin(); scope.focus(); }
        Text {
            x: 10px;
            width: parent.width - 20px;
            height: parent.height;
            text: root.capturing ? GuiI18n.text("gui-minecraft-press-input") : root.text;
            color: root.capturing ? Theme.accent : Theme.text;
            font-size: Theme.body-size;
            font-weight: root.capturing ? 600 : 400;
            vertical-alignment: center;
            overflow: elide;
        }
        scope := FocusScope {
            width: 100%;
            height: 100%;
            key-pressed(event) => {
                if (!root.capturing) {
                    return reject;
                }
                root.captured-key(event.text);
                return accept;
            }
            changed has-focus => {
                if (!self.has-focus && root.capturing) {
                    root.cancel();
                }
            }
            TouchArea {
                width: 100%;
                height: 100%;
                mouse-cursor: pointer;
                pointer-event(event) => {
                    if (event.kind != PointerEventKind.down) {
                        return;
                    }
                    scope.focus();
                    if (root.capturing) {
                        root.captured-mouse(
                            event.button == PointerEventButton.left ? "left"
                            : event.button == PointerEventButton.right ? "right"
                            : event.button == PointerEventButton.middle ? "middle"
                            : event.button == PointerEventButton.back ? "back"
                            : event.button == PointerEventButton.forward ? "forward"
                            : "other");
                    } else if (event.button == PointerEventButton.left) {
                        root.begin();
                    }
                }
            }
        }
    }

    // One catalog-driven preference row: its identity and state on the first
    // line, its editor on the second, and any reason it will not be written
    // underneath.
    component MinecraftPreferenceRowCard inherits Rectangle {
        in property <MinecraftPreferenceRow> item;
        in property <bool> compact;
        in property <bool> capturing;
        in property <bool> languages-available;
        in property <[string]> aspect-labels;
        callback set-boolean(string, bool);
        callback set-number(string, float);
        callback edit-number(string, string);
        callback commit-number(string, string);
        callback set-enumeration(string, int);
        callback set-language(string, int);
        callback edit-window(string, string, string);
        callback commit-window-width(string, string, string);
        callback commit-window-height(string, string, string);
        callback set-window-aspect(string, int);
        callback set-window-lock(string, bool);
        callback open-window-preview(string);
        callback commit-text(string, string);
        callback forget(string);
        callback begin-capture(string);
        callback cancel-capture();
        callback captured-key(string, string);
        callback captured-mouse(string, string);
        callback retry-languages();
        property <length> heading-height: root.item.section-heading == "" ? 0px : 42px * Theme.text-scale;
        property <length> detail-lines: ((root.item.invalid ? 22px : 0px) + (root.item.notice == "" ? 0px : 22px) + (root.item.status-detail == "" ? 0px : 22px)) * Theme.text-scale;
        // The window size row spends a second line on the ratio it projects
        // with, which is the one editor that does not fit on one.
        property <length> editor-lines: root.item.editor == "window" ? 42px * Theme.text-scale : 0px;
        height: root.heading-height + 104px * Theme.text-scale + root.editor-lines + root.detail-lines;
        background: transparent;
        VerticalLayout {
            spacing: 0px;
            if root.item.section-heading != "" : Rectangle {
                height: root.heading-height;
                VerticalLayout {
                    alignment: end;
                    padding-bottom: 7px;
                    Eyebrow { text: root.item.section-heading; }
                }
            }
            Rectangle {
                border-radius: 8px;
                border-width: 1px;
                border-color: root.item.dirty ? Theme.accent.with-alpha(0.65) : Theme.border;
                background: Theme.surface;
                VerticalLayout {
                    padding: 12px;
                    spacing: 6px;
                    // No explicit alignment on either line: an aligned layout
                    // keeps every child at its preferred width, which would
                    // collapse the stretching label column and push the row's
                    // contents into a huddle at one edge.
                    HorizontalLayout {
                        spacing: 10px;
                        VerticalLayout {
                            horizontal-stretch: 1;
                            alignment: center;
                            spacing: 1px;
                            Text { text: root.item.label; color: Theme.text; font-size: Theme.row-heading-size; font-weight: 600; vertical-alignment: center; overflow: elide; }
                            Text { text: root.item.technical-id; color: Theme.text-muted; font-size: Theme.caption-size; font-family: Theme.mono-family; overflow: elide; }
                        }
                        // Every badge in one cluster on one baseline. Split
                        // across the label column and this one they sat at
                        // different heights, and an override read as a second
                        // compatibility verdict.
                        VerticalLayout { alignment: center; horizontal-stretch: 0;
                            ActionRow {
                                // Muted: a tier is a classification, not a
                                // state, and it must not compete with the three
                                // badges beside it that are.
                                if root.item.advanced : Pill { text: GuiI18n.text("gui-minecraft-advanced"); tone-color: Theme.text-muted; }
                                // Not the compatibility badge's green: what
                                // Ephervex writes and what a release supports
                                // are different sentences.
                                if root.item.overridden : Pill { text: GuiI18n.text("gui-minecraft-state-overridden"); tone-color: Theme.accent-secondary; }
                                if root.item.dirty : Pill { text: GuiI18n.text("gui-minecraft-unsaved"); tone-color: Theme.accent; }
                                CompatibilityBadge { text: root.item.status; kind: root.item.status-kind; detail: root.item.status-detail; }
                            }
                        }
                    }
                    HorizontalLayout {
                        spacing: 10px;
                        if root.item.editor == "toggle" : Toggle {
                            accessible-name: root.item.label;
                            checked: root.item.bool-value;
                            toggled(value) => { root.set-boolean(root.item.id, value); }
                        }
                        if root.item.editor == "number" : number-field := TextField {
                            width: root.compact ? 150px : 190px;
                            accessible-name: root.item.label;
                            invalid: root.item.invalid;
                            input-type: InputType.text;
                            text: root.item.text-value;
                            edited(value) => { root.edit-number(root.item.id, value); }
                            accepted => { root.commit-number(root.item.id, number-field.text); }
                            focus-lost => { root.commit-number(root.item.id, number-field.text); }
                        }
                        if root.item.editor == "slider" : Slider {
                            width: root.compact ? 170px : 240px;
                            accessible-label: root.item.label;
                            minimum: root.item.number-min;
                            maximum: root.item.number-max;
                            step: root.item.number-step;
                            // Committed on release rather than on every
                            // movement: the row re-renders when the draft
                            // changes, and a control that rebuilt itself
                            // mid-drag would drop the drag.
                            value: root.item.number-value;
                            released(value) => { root.set-number(root.item.id, value); }
                        }
                        // Wide enough for the longest reading a slider
                        // produces, so a named endpoint is never cut in half.
                        if root.item.editor == "slider" : BodyText { text: root.item.number-readout; color: Theme.text; width: 150px; }
                        // Shown only where the reading says more than the
                        // number itself does, such as a scale of zero that
                        // means "pick one to fit the window".
                        if root.item.editor == "number" && root.item.number-hint != "" : BodyText { text: root.item.number-hint; color: Theme.text; horizontal-stretch: 0; }
                        if root.item.editor == "enum" : enum-box := ComboBox {
                            width: root.compact ? 210px : 280px;
                            accessible-label: root.item.label;
                            model: root.item.enum-labels;
                            current-index: root.item.enum-index;
                            selected => { root.set-enumeration(root.item.id, enum-box.current-index); }
                        }
                        if root.item.editor == "language" && root.languages-available : language-box := ComboBox {
                            width: root.compact ? 240px : 330px;
                            accessible-label: root.item.label;
                            model: root.item.enum-labels;
                            current-index: root.item.enum-index;
                            selected => { root.set-language(root.item.id, language-box.current-index); }
                        }
                        if root.item.editor == "language" && !root.languages-available : AppButton {
                            text: GuiI18n.text("gui-minecraft-language-retry");
                            clicked => { root.retry-languages(); }
                        }
                        if root.item.editor == "keybinding" : PreferenceKeyBindingEditor {
                            text: root.item.binding-label;
                            capturing: root.capturing;
                            accessible-name: root.item.label;
                            begin => { root.begin-capture(root.item.id); }
                            cancel => { root.cancel-capture(); }
                            captured-key(reported) => { root.captured-key(root.item.id, reported); }
                            captured-mouse(reported) => { root.captured-mouse(root.item.id, reported); }
                        }
                        // One product preference, so both numbers are read
                        // and committed together: half a window size is not a
                        // window size. The ratio and the lock are the same
                        // value's other face, not a second one.
                        if root.item.editor == "window" : VerticalLayout {
                            spacing: 8px;
                            HorizontalLayout {
                                spacing: 8px;
                                FieldLabel { text: GuiI18n.text("gui-minecraft-width"); vertical-alignment: center; }
                                width-field := TextField {
                                    width: 96px;
                                    accessible-name: GuiI18n.text("gui-minecraft-width");
                                    invalid: root.item.invalid;
                                    text: root.item.text-value;
                                    edited(value) => { root.edit-window(root.item.id, value, height-field.text); }
                                    // The edited field is the one the ratio
                                    // holds still, so each field commits on
                                    // its own axis.
                                    accepted => { root.commit-window-width(root.item.id, width-field.text, height-field.text); }
                                    focus-lost => { root.commit-window-width(root.item.id, width-field.text, height-field.text); }
                                }
                                FieldLabel { text: GuiI18n.text("gui-minecraft-height"); vertical-alignment: center; }
                                height-field := TextField {
                                    width: 96px;
                                    accessible-name: GuiI18n.text("gui-minecraft-height");
                                    invalid: root.item.invalid;
                                    text: root.item.height-text;
                                    edited(value) => { root.edit-window(root.item.id, width-field.text, value); }
                                    accepted => { root.commit-window-height(root.item.id, width-field.text, height-field.text); }
                                    focus-lost => { root.commit-window-height(root.item.id, width-field.text, height-field.text); }
                                }
                                Rectangle { horizontal-stretch: 1; min-width: 0px; }
                            }
                            // The second line is the same value by eye: the
                            // ratio it is held to, and the window that is it.
                            // Keeping it off the first line leaves the row's
                            // own actions their column at every width.
                            HorizontalLayout {
                                spacing: 8px;
                                aspect-box := ComboBox {
                                    width: root.compact ? 116px : 136px;
                                    accessible-label: GuiI18n.text("gui-minecraft-aspect");
                                    model: root.aspect-labels;
                                    current-index: root.item.aspect-index;
                                    selected => { root.set-window-aspect(root.item.id, aspect-box.current-index); }
                                }
                                FieldLabel { text: GuiI18n.text("gui-minecraft-aspect-lock"); vertical-alignment: center; }
                                Toggle {
                                    accessible-name: GuiI18n.text("gui-minecraft-aspect-lock");
                                    checked: root.item.aspect-locked;
                                    toggled(value) => { root.set-window-lock(root.item.id, value); }
                                }
                                // Sizing by eye means a window the size of the
                                // window, not a drawing of one.
                                AppButton {
                                    text: GuiI18n.text("gui-minecraft-window-open");
                                    clicked => { root.open-window-preview(root.item.id); }
                                }
                                Rectangle { horizontal-stretch: 1; min-width: 0px; }
                            }
                        }
                        if root.item.editor == "text" : text-field := TextField {
                            width: root.compact ? 210px : 280px;
                            accessible-name: root.item.label;
                            text: root.item.text-value;
                            accepted => { root.commit-text(root.item.id, text-field.text); }
                            focus-lost => { root.commit-text(root.item.id, text-field.text); }
                        }
                        Rectangle { horizontal-stretch: 1; min-width: 8px; }
                        // Never elided: which of absent, chosen, and applied
                        // a row is in is the sentence this screen exists to
                        // make legible, and the spacer beside it can shrink.
                        if root.item.state-text != "" : BodyText { text: root.item.state-text; color: Theme.text-muted; horizontal-stretch: 0; }
                        if root.item.can-reset : AppButton {
                            text: GuiI18n.text("gui-minecraft-forget");
                            min-width: 116px;
                            clicked => { root.forget(root.item.id); }
                        }
                    }
                    if root.item.invalid : Text { text: root.item.validation; color: Theme.danger; font-size: Theme.caption-size; overflow: elide; }
                    if root.item.notice != "" : Text { text: root.item.notice; color: Theme.warning; font-size: Theme.caption-size; overflow: elide; }
                    if root.item.status-detail != "" : Text { text: root.item.status-detail; color: Theme.text-muted; font-size: Theme.caption-size; overflow: elide; }
                }
            }
        }
    }

    component PendingCard inherits Rectangle {
        in property <PendingEntityRow> item;
        height: Theme.card-height;
        border-radius: 8px;
        border-width: 1px;
        border-color: Theme.accent.with-alpha(0.7);
        background: Theme.surface;
        HorizontalLayout { padding: 15px; spacing: 16px; cross-axis-alignment: center;
            LoaderGlyph { kind: item.loader-kind; server: item.server; }
            VerticalLayout { horizontal-stretch: 1; spacing: 5px;
                Heading { text: item.name; }
                BodyText { text: item.target; wrap: word-wrap; }
                BodyText { text: item.detail != "" ? item.detail : GuiI18n.text("gui-creation-pending-detail"); color: Theme.text-muted; wrap: word-wrap; overflow: elide; }
                // Only where the work knows how far along it is. An
                // indeterminate stage keeps the spinner and says nothing it
                // cannot back up.
                if !item.indeterminate : Rectangle { height: 4px; border-radius: 2px; background: Theme.border; Rectangle { x: 0px; width: parent.width * item.progress; height: parent.height; border-radius: 2px; background: Theme.accent; } }
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
        HorizontalLayout { padding-left: 16px; padding-right: 10px; padding-top: 10px; padding-bottom: 10px; spacing: 10px; cross-axis-alignment: center;
            VerticalLayout { horizontal-stretch: 1; spacing: 3px; Heading { text: root.error ? GuiI18n.text("gui-notification-error") : GuiI18n.text("gui-notification-complete"); font-size: Theme.body-size; } toast-message := BodyText { text: root.message; color: Theme.text-secondary; wrap: word-wrap; } }
            IconButton { icon: @image-url("../../../assets/icons/close.svg"); accessible-name: GuiI18n.text("gui-dismiss-notification"); width: 32px; height: 32px; clicked => { root.dismiss(); } }
        }
    }

    // One thing the launcher is running.
    //
    // Not an activity card: that one describes an operation with a progress
    // fraction and a Cancel. A running game has neither, and the two answer
    // different questions.
    component RunningCard inherits Rectangle {
        in property <RunningRow> item;
        callback open-log(string, string);
        callback open-console(string);
        callback open-workspace(string, string);
        callback stop(string, string);
        callback force-stop(string, string);
        height: 92px;
        border-radius: 8px;
        border-width: 1px;
        border-color: Theme.border;
        background: Theme.surface;
        // Declared before the buttons, so a click on one of them reaches the
        // button rather than the card behind it.
        //
        // Opening on a double click, like every other card in the application,
        // and for a reason this page makes sharper: a disabled control does not
        // consume its click, so a single-click card would navigate away from a
        // press on a Stop that is greyed out.
        TouchArea {
            mouse-cursor: pointer;
            double-clicked => { root.open-workspace(root.item.kind, root.item.id); }
        }
        HorizontalLayout {
            padding: 12px;
            spacing: 12px;
            cross-axis-alignment: center;
            VerticalLayout {
                alignment: center;
                LoaderGlyph { server: root.item.kind == "server"; kind: "vanilla"; }
            }
            VerticalLayout {
                alignment: center;
                spacing: 3px;
                horizontal-stretch: 1;
                HorizontalLayout {
                    spacing: 8px;
                    Text { text: root.item.name; color: Theme.text; font-size: Theme.row-heading-size; font-weight: 600; overflow: elide; horizontal-stretch: 1; vertical-alignment: center; }
                    VerticalLayout { alignment: center; Pill { text: root.item.kind == "server" ? GuiI18n.text("gui-server") : GuiI18n.text("gui-instance"); tone-color: root.item.kind == "server" ? Theme.server : Theme.accent; } }
                }
                // Exact, because a pid is what a developer chases outside the
                // launcher and a rounded one would be useless.
                BodyText {
                    text: (root.item.pid != "" ? "pid " + root.item.pid + "  |  " : "") + root.item.uptime + (root.item.usage != "" ? "  |  " + root.item.usage : "");
                    color: Theme.text-secondary;
                    font-size: Theme.caption-size;
                    overflow: elide;
                }
                Text {
                    text: root.item.last-line;
                    color: Theme.text-muted;
                    font-size: Theme.caption-size;
                    font-family: Theme.mono-family;
                    overflow: elide;
                }
            }
            VerticalLayout {
                alignment: center;
                HorizontalLayout {
                    spacing: 8px;
                    AppButton { text: GuiI18n.text("gui-open-log"); clicked => { root.open-log(root.item.kind, root.item.id); } }
                    if root.item.kind == "server" : AppButton { text: GuiI18n.text("gui-open-console"); clicked => { root.open-console(root.item.id); } }
                    AppButton { text: root.item.kind == "server" ? GuiI18n.text("gui-stop") : GuiI18n.text("gui-stop-game"); tone: "danger"; enabled: root.item.can-stop; clicked => { root.stop(root.item.kind, root.item.id); } }
                    // The bigger hammer, and only for a client: a launch left
                    // by another run of the launcher has nobody supervising it,
                    // so cancelling is not available and the instance would
                    // otherwise stay stuck claiming to be busy.
                    if root.item.kind == "client" : AppButton { text: GuiI18n.text("gui-force-stop"); tone: "danger"; clicked => { root.force-stop(root.item.id, root.item.launch); } }
                }
            }
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
                BodyText { text: item.id + " | " + item.time; color: Theme.text-muted; font-family: Theme.mono-family; font-size: Theme.caption-size; }
                if item.active && !item.indeterminate : Rectangle { height: 4px; border-radius: 2px; background: Theme.border; Rectangle { x: 0px; width: parent.width * item.progress; height: parent.height; border-radius: 2px; background: Theme.accent; } }
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
        callback kill(string);
        callback open-workspace(string);
        callback open-folder(string);
        callback create-server(string, string);
        callback duplicate(string);
        callback delete(string, string);
        height: Theme.card-height;
        border-radius: 8px;
        border-width: selected ? 2px : 1px;
        border-color: selected ? Theme.accent : Theme.border;
        background: selected ? #17272c : card-area.has-hover ? Theme.elevated : Theme.surface;
        accessible-role: button;
        accessible-label: item.name;
        accessible-action-default => { root.choose(item.id); }
        // First, so it is *under* the card's own buttons rather than over
        // them. A touch area declared after them takes the press first, and one
        // that sets a width without an x is centered rather than left-aligned,
        // which put it across the left of the Launch button and swallowed every
        // click that landed on the label.
        card-area := TouchArea {
            clicked => { root.choose(item.id); }
            double-clicked => { root.open-workspace(item.id); }
        }
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
                    if !item.can-launch && item.can-kill : AppButton { width: 150px; text: GuiI18n.text("gui-kill-instance"); tone: "danger"; clicked => { root.kill(item.id); } }
                    if !item.can-launch && !item.can-kill : Rectangle { width: 150px; }
                    IconButton { icon: @image-url("../../../assets/icons/open-workspace.svg"); accessible-name: GuiI18n.text("gui-open-workspace"); clicked => { root.open-workspace(item.id); } }
                }
            }
        }
        ContextMenuArea {
            Menu {
                MenuItem { title: GuiI18n.text("gui-open-workspace"); activated => { root.open-workspace(item.id); } }
                MenuItem { title: GuiI18n.text("gui-launch"); enabled: item.can-launch; activated => { root.launch(item.id); } }
                MenuItem { title: GuiI18n.text("gui-kill-instance"); enabled: item.can-kill; activated => { root.kill(item.id); } }
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
        height: Theme.card-height;
        border-radius: 8px;
        border-width: selected ? 2px : 1px;
        border-color: selected ? Theme.accent : Theme.border;
        background: selected ? #17272c : card-area.has-hover ? Theme.elevated : Theme.surface;
        accessible-role: button;
        accessible-label: item.name;
        accessible-action-default => { root.choose(item.id); }
        // Under the card's buttons, not over them. See `InstanceCard`.
        card-area := TouchArea {
            clicked => { root.choose(item.id); }
            double-clicked => { root.open-workspace(item.id); }
        }
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
        // The accounts a launch can be made as, for the control beside Launch.
        in property <[AccountRow]> accounts;
        callback launch(string);
        callback launch-as(string, string, bool);
        callback add-account();
        callback kill(string);
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
            // A paired row is split down the middle rather than by label
            // length. A layout gives each child its preferred width and shares
            // only the surplus, so two buttons whose captions differ come out
            // visibly unequal and the block stops reading as a grid.
            //
            // Launch as keeps its place while the instance is running, because
            // that is exactly when a second account is worth launching. What
            // changes is what it means: a second client beside the first.
            if item.can-launch || item.can-kill : HorizontalLayout { spacing: 8px;
                if item.can-launch : AppButton { width: (parent.width - 8px) / 2; text: GuiI18n.text("gui-launch"); tone: "primary"; clicked => { root.launch(item.id); } }
                if item.can-kill : AppButton { width: (parent.width - 8px) / 2; text: GuiI18n.text("gui-kill-instance"); tone: "danger"; clicked => { root.kill(item.id); } }
                LaunchAsButton { width: (parent.width - 8px) / 2; accounts: root.accounts; alongside: item.can-kill; launch-as(account, alongside) => { root.launch-as(item.id, account, alongside); } add-account => { root.add-account(); } }
            }
            if item.connect-server-id != "" : AppButton { text: GuiI18n.value("gui-launch-connect", item.connect-server-name); enabled: item.can-connect; clicked => { root.connect(item.id, item.connect-server-id); } }
            HorizontalLayout { spacing: 8px;
                AppButton { width: (parent.width - 8px) / 2; text: GuiI18n.text("gui-open-workspace"); clicked => { root.workspace(item.id); } }
                AppButton { width: (parent.width - 8px) / 2; text: GuiI18n.text("gui-open-folder"); clicked => { root.open-folder(item.id); } }
            }
            AppButton { text: GuiI18n.text("gui-create-linked-server"); clicked => { root.create-server(item.id, item.name); } }
            Divider { }
            Eyebrow { text: GuiI18n.text("gui-diagnostics"); }
            AppButton { text: GuiI18n.text("gui-view-logs-crash-reports"); clicked => { root.diagnostics(item.id); } }
        }
    }

    // A window the exact size of the Minecraft window being configured.
    //
    // This is the picker: it is sized by the window manager like any other
    // window, and whatever size it is dragged to is the exact size stored.
    // Nothing is scaled, so what the user sees on screen is the window they
    // are asking Minecraft for. The size it reports is in the same screen
    // coordinates Minecraft's own window uses.
    export component WindowSizePreview inherits Window {
        in property <string> readout;
        // Whether a ratio is being held, which is the difference between a
        // window that follows the pointer and one that settles onto a shape.
        in property <bool> holds-ratio;
        // The smallest window this build accepts, supplied by the code that
        // owns that rule rather than repeated here as a number.
        in property <length> minimum-edge;
        callback resized(length, length);
        callback dismissed();
        title: GuiI18n.text("gui-minecraft-window-preview-title");
        // Sizing a window against a settings page it would otherwise hide
        // behind.
        always-on-top: true;
        background: Theme.background;
        // Every edge and corner stays draggable. A held ratio reaches the
        // window manager as an aspect, not as a size this window refuses to
        // take, so the manager moves the edge the user is not holding rather
        // than this window losing half its handles.
        min-width: root.minimum-edge;
        min-height: root.minimum-edge;
        changed width => { root.resized(self.width, self.height); }
        changed height => { root.resized(self.width, self.height); }
        VerticalLayout {
            padding: 16px;
            spacing: 10px;
            alignment: center;
            Text {
                text: root.readout;
                color: Theme.text;
                font-size: 32px;
                horizontal-alignment: center;
                overflow: elide;
            }
            Text {
                text: root.holds-ratio
                    ? GuiI18n.text("gui-minecraft-window-preview-hint-ratio")
                    : GuiI18n.text("gui-minecraft-window-preview-hint");
                color: Theme.text-secondary;
                font-size: Theme.caption-size;
                horizontal-alignment: center;
                wrap: word-wrap;
            }
            HorizontalLayout {
                alignment: center;
                AppButton {
                    text: GuiI18n.text("gui-minecraft-window-preview-close");
                    tone: "primary";
                    clicked => { root.dismissed(); }
                }
            }
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
        in-out property <[ServerRow]> global-servers;
        // What the launcher is running right now. Polled only while the page
        // showing it is open.
        in-out property <[RunningRow]> running;
        in-out property <[ServerRow]> linked-servers;
        in-out property <[ModRow]> managed-mods;
        // Empty unless something has to be said about how complete the list is.
        in-out property <string> managed-mods-notice;
        // A server's own list, kept apart from its linked client's. The two are
        // separate installations that happen to share a pack, and a server that
        // had a client-only mod turned off must not look like the client did.
        in-out property <[ModRow]> server-mods;
        in-out property <string> server-mods-notice;
        // -- what the selected host's newest crash report says. Empty report
        // means it has never crashed, which is what clears a notice left by a
        // previous selection.
        in-out property <string> crash-report;
        in-out property <string> crash-failure;
        in-out property <bool> crash-client-code-on-server;
        in-out property <int> crash-suggested;
        // -- managing one mod, which is what the mod modals act on.
        in-out property <string> mod-action-path;
        in-out property <string> mod-action-name;
        in-out property <string> mod-action-installed;
        in-out property <string> mod-action-notice;
        in-out property <bool> mod-action-can-change: false;
        in-out property <bool> mod-action-can-update: false;
        in-out property <bool> mod-action-can-remove: false;
        in-out property <bool> mod-action-up-to-date: false;
        in-out property <bool> mod-action-loading: false;
        // Set the moment a mutation is submitted and cleared when it answers,
        // so one click cannot become two operations.
        in-out property <bool> mod-action-pending: false;
        in-out property <string> mod-action-selected;
        in-out property <[ModVersionRow]> mod-versions;
        in-out property <[string]> mod-removal-dependencies;
        in-out property <bool> mod-removal-unprovable: false;
        // -- carrying the same mod decision to this client's linked servers.
        in-out property <[LinkedTargetRow]> linked-targets;
        in-out property <string> linked-support;
        in-out property <string> linked-path;
        in-out property <string> linked-version;
        in-out property <string> linked-name;
        in-out property <string> linked-version-number;
        in-out property <[LinkedTargetRow]> linked-results;
        in-out property <string> linked-result-title;
        in-out property <string> linked-result-summary;
        // -- changing the loader build this instance runs.
        in-out property <string> loader-change-current;
        in-out property <string> loader-change-selected;
        in-out property <[string]> loader-change-versions;
        in-out property <bool> loader-change-loading: false;
        in-out property <bool> loader-change-pending: false;
        in-out property <int> loader-change-unverifiable: 0;
        callback open-loader-change();
        callback change-loader-version(string, string);
        callback open-mod-manager(string);
        // (host kind, host id, the mod's path, whether the loader loads it)
        callback set-mod-enabled(string, string, string, bool);
        callback change-mod-version(string, string, string);
        callback plan-mod-removal(string, string);
        // Works out what a change or an update would do here and on every
        // linked server. An empty version means "the newest release that fits
        // this client", which is a different request from an exact choice.
        callback plan-mod-apply(string, string, string);
        callback remove-mod(string, string);
        in-out property <[ModSearchRow]> mod-results;
        in-out property <[DiagnosticRow]> diagnostics;
        // Whether the Logs tab is showing saved log files rather than the one
        // being written. Beside `selected-diagnostic-kind` rather than folded
        // into it, because that value names a kind of file for the services
        // that read one and must stay exactly "log" or "crash-report".
        in-out property <bool> log-shows-files;
        in-out property <[ActivityRow]> activities;
        in-out property <[AccountRow]> accounts;
        in-out property <[JavaRow]> java-runtimes;
        // -- Minecraft defaults for future client instances.
        in-out property <[MinecraftPreferenceRow]> minecraft-preference-rows;
        in-out property <[string]> minecraft-categories;
        // The ratios the window size picker offers, in the order it lists them.
        in-out property <[string]> minecraft-aspect-labels;
        in-out property <int> minecraft-category-index: 0;
        in-out property <string> minecraft-query;
        in-out property <bool> minecraft-show-advanced: false;
        in-out property <bool> minecraft-only-overridden: false;
        // The row waiting for a key or mouse press, by product id. Kept out of
        // the row model on purpose: entering capture must not rebuild the
        // rows, because rebuilding them would destroy the control that just
        // took focus.
        in-out property <string> minecraft-capturing-id;
        in-out property <bool> minecraft-languages-available: false;
        in-out property <string> minecraft-preview-release;
        in-out property <bool> minecraft-dirty: false;
        in-out property <bool> minecraft-can-save: false;
        in-out property <int> minecraft-dirty-count: 0;
        in-out property <int> minecraft-total-count: 0;
        in-out property <string> minecraft-preserved-notice;
        in-out property <string> minecraft-unusable-notice;
        in-out property <bool> minecraft-saving: false;
        in-out property <[PendingEntityRow]> pending-instances;
        in-out property <[PendingEntityRow]> pending-servers;
        in-out property <[PendingEntityRow]> linked-pending-servers;
        in-out property <[LogChunk]> log-chunks;
        in-out property <bool> log-has-content: false;
        in-out property <string> log-status;
        in-out property <string> log-query;
        in-out property <bool> log-wrap-lines: false;
        in-out property <bool> log-follow: true;
        in-out property <bool> log-history-truncated: false;
        in-out property <int> log-pending-lines: 0;
        in-out property <int> log-content-columns: 80;
        in-out property <int> log-revision: 0;
        // What the pointer has selected across the console's rows, in reading
        // order, as byte offsets into each end's own row. -1 is no selection.
        in-out property <int> log-selection-start-row: -1;
        in-out property <int> log-selection-start-offset;
        in-out property <int> log-selection-end-row: -1;
        in-out property <int> log-selection-end-offset;
        in-out property <bool> log-has-selection;
        in-out property <int> log-selection-revision: 0;
        in-out property <bool> log-trace-enabled: true;
        in-out property <bool> log-debug-enabled: true;
        in-out property <bool> log-info-enabled: true;
        in-out property <bool> log-warn-enabled: true;
        in-out property <bool> log-error-enabled: true;
        in-out property <bool> log-fatal-enabled: true;
        in-out property <bool> log-other-enabled: true;
        in-out property <string> selected-instance-id;
        in-out property <string> selected-server-id;
        in-out property <string> selected-global-server-id;
        in-out property <int> selected-instance-index: -1;
        in-out property <int> selected-server-index: -1;
        in-out property <int> selected-global-server-index: -1;
        in-out property <string> instance-filter: "all";
        in-out property <string> instance-query;
        in-out property <string> server-query;
        in-out property <string> mod-query;
        // The query a search was actually submitted with. Distinct from
        // `mod-query`, which follows the field as it is typed: an answer is
        // matched against what was asked for, not against what the field says
        // by the time it arrives.
        in-out property <string> mod-search-submitted;
        // Which catalog the browser is reading. Explicit and visible: a project
        // id belongs to one catalog, and a result list that did not say where
        // it came from would be unreadable once two of them are offered.
        in-out property <string> mod-provider: "modrinth";
        // The catalog a submitted search was for. Matched against an answer for
        // the same reason `mod-search-submitted` is: switching catalog asks a
        // new question, and the previous one's answer must not land on it.
        in-out property <string> mod-search-provider;
        in-out property <bool> mod-search-loading: false;
        // True once a search has been submitted, so an empty list can say "no
        // results" rather than being mistaken for "nothing asked yet".
        in-out property <bool> mod-search-ran: false;
        // How many results the catalog says match. Informational only: what
        // decides whether "Load more" is offered is `mod-results-next-offset`,
        // not this count.
        in-out property <int> mod-results-total: 0;
        // Where the provider says its next page begins, or -1 once there is
        // none. Set from the provider's own answer, never from how many rows
        // ended up on screen: a page can consume results this launcher
        // declines to show, and the next request has to continue from where
        // the provider left off, not from the visible list's length.
        in-out property <int> mod-results-next-offset: -1;
        // The offset the browser is currently waiting for a page at. Set the
        // moment a search or a "load more" is sent, so a page that lands after
        // the browser has moved on to a different offset is recognized as
        // stale rather than judged by how many rows happen to be on screen.
        in-out property <int> mod-search-requested-offset: 0;
        // An opaque id minted for the exact request currently in flight, set
        // the moment it is sent and cleared on catalog switch. Never
        // interpreted here: instance, catalog, submitted text, and offset can
        // all agree between two distinct requests, and this is what still
        // tells them apart.
        in-out property <string> mod-search-expected-request-id: "";
        // How many results one Search or one Load More aims to put on screen.
        // Not a provider page size: a provider is asked in pages of at most
        // what one request may take, and a larger choice is filled by asking
        // again.
        in-out property <int> mod-page-size: 10;
        // The same number as the picker spells it. Kept beside the count
        // because a combo box selects by its own text and the fill arithmetic
        // needs a number.
        in-out property <string> mod-page-size-label: "10";
        in-out property <[string]> mod-page-sizes: ["10", "25", "50", "100"];
        // How many rows the fill currently under way is aiming for, counted
        // over the whole list: a Load More adds another page size to whatever
        // is already shown.
        in-out property <int> mod-results-target: 10;
        in-out property <string> mod-search-notice;
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
        // Whether a file from the desktop is being held over the window. Set
        // only while the drop would be accepted, so the highlight never
        // promises something a release would not do.
        in-out property <bool> file-drop-active: false;
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
        // How many servers go with the instance the delete dialog is about, so
        // "Delete Files" is never a surprise.
        in-out property <int> pending-action-linked-servers: 0;
        in-out property <bool> show-inspector: true;
        in-out property <bool> compact-layout;
        in-out property <string> mod-plan-instance;
        // The reference the open review asked about, so an answer to an
        // earlier one cannot replace it.
        in-out property <string> mod-plan-requested;
        // The catalog the open review asked. Part of the identity an answer is
        // matched against, and what an install is sent with.
        in-out property <string> mod-plan-requested-provider;
        // Whether the plan being awaited was asked for by the browser's Install
        // button. Such a plan installs itself the moment it comes back with
        // nothing left for the user to decide; the confirmation opens only when
        // there is a choice, which today means linked servers to target.
        in-out property <bool> mod-plan-direct: false;
        in-out property <string> mod-plan-provider;
        in-out property <string> mod-plan-project;
        // The exact release the confirmation showed, which is the release
        // installing it asks for. A newer one published since must not take its
        // place, here or on any selected server.
        in-out property <string> mod-plan-version-id;
        in-out property <string> mod-plan-title;
        in-out property <string> mod-plan-version;
        in-out property <int> mod-plan-install-count;
        in-out property <int> mod-plan-dependency-count;
        in-out property <int> mod-plan-optional-count;
        in-out property <int> mod-plan-existing-count;
        in-out property <int> mod-plan-warning-count;

        // Dialog form state.
        // What the user typed, which is empty until they type. The launcher's
        // own suggestion is shown as the placeholder beside it and is what a
        // field left empty means, so a suggestion can never go stale against
        // the release and loader actually selected.
        in-out property <string> create-name;
        in-out property <string> create-name-suggestion;
        in-out property <[string]> minecraft-releases;
        // The releases pinned to the front of every version list, in the order
        // the user recorded them.
        in-out property <[string]> favorite-minecraft-versions;
        // Whether the release the New Instance dialog currently offers is one
        // of them. Kept as its own value because Slint cannot ask a model
        // whether it contains something.
        in-out property <bool> create-minecraft-favorite: false;
        in-out property <[string]> create-loader-versions;
        in-out property <[string]> create-server-loader-versions;
        in-out property <string> create-minecraft;
        in-out property <string> create-loader: "Vanilla";
        // The loaders offered for the release currently selected. Narrowed in
        // Rust rather than written out here, because which loader ever existed
        // for a Minecraft release is a fact `ephervex-core` owns.
        in-out property <[string]> create-loaders: ["Vanilla", "Forge", "NeoForge"];
        in-out property <string> create-loader-version;
        in-out property <bool> create-persistent: false;
        in-out property <string> import-path;
        in-out property <string> import-name;
        // Whether the import should also leave a server behind. Off by
        // default: most imports are opened to reproduce a client.
        in-out property <bool> import-create-server: false;
        // "idle" | "analyzing" | "resolving" | "ready" | "failed" | "submitting".
        // Drives which part of the import modal is visible and whether the
        // archive picker and the Import button are enabled.
        in-out property <string> import-status-kind: "idle";
        in-out property <string> import-format;
        // Pre-composed, already-localized lines: the modal displays them as
        // plain text and does not itself run any of this through GuiI18n.
        in-out property <string> import-pack-header;
        in-out property <string> import-author-line;
        in-out property <string> import-minecraft-line;
        in-out property <string> import-loader-line;
        in-out property <string> import-required-line;
        in-out property <string> import-optional-line;
        in-out property <[ImportOptionRow]> import-optional-items;
        in-out property <[ImportBlockedRow]> import-blocked-items;
        // Whether the current archive has a successful, fully resolved
        // analysis: the only condition the Import button's `enabled` needs.
        in-out property <bool> import-ready;
        // "missing-credential" | "rejected-credential" | "file-unavailable" |
        // "distribution-restricted" | "malformed-pack" | "unsupported-loader"
        // | "other". Classified from the core error's stable id (never from
        // `modal-error`'s text), and only consulted while `import-status-kind`
        // is "failed": it decides whether the modal offers a state-specific
        // affordance alongside the error text.
        in-out property <string> import-error-kind: "other";
        in-out property <string> create-server-client-id;
        in-out property <string> create-server-name;
        // What the name field shows when it is empty, and what an untouched
        // dialog creates. A client's default name with "Server" after it: a
        // linked server borrows the client's own name, a standalone one is
        // named for its exact target the same way a new instance is.
        in-out property <string> create-server-name-suggestion;
        // What the two mod lists are filtered by. Filtering happens in Rust so
        // the list a `for` builds is only the rows that match, which is also
        // what keeps a large pack from laying out every card to show three.
        in-out property <string> managed-mods-query;
        in-out property <string> server-mods-query;
        callback filter-mods(string, string);
        in-out property <string> create-server-minecraft;
        in-out property <string> create-server-loader: "Vanilla";
        in-out property <[string]> create-server-loaders: ["Vanilla", "Forge", "NeoForge"];
        in-out property <string> create-server-loader-version;
        in-out property <bool> create-server-persistent: false;
        in-out property <bool> create-server-automatic-port: true;
        in-out property <string> create-server-port: "25565";
        // The archive a server's content comes from, empty when the server is
        // built the usual way. Holds the full path because that is what the
        // backend needs; the modal shows only the file name.
        in-out property <string> create-server-pack;
        in-out property <string> create-server-pack-name;
        // The same choice for the server an import builds alongside its client.
        // Separate from the create-server dialog's, because both can be open in
        // one session and neither is the other's default.
        in-out property <string> import-server-pack;
        in-out property <string> import-server-pack-name;
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
        // Whether a CurseForge API key is in the keyring. The only thing the
        // view is told about it: there is no property the value could arrive
        // in, so no snapshot can push a stored secret back into the window.
        in-out property <bool> curseforge-key-configured;
        // What the user is typing right now, and nothing else. Cleared on
        // submit and never written from a snapshot.
        in-out property <string> curseforge-api-key;
        // Which catalog the mod browser opens on, as the settings file records
        // it. Written only from a snapshot of what is stored, so a refused save
        // leaves the previous choice selected rather than the attempted one.
        in-out property <string> default-mod-catalog;
        // The recorded name exactly as stored, shown when it is a catalog this
        // build cannot resolve.
        in-out property <string> configured-mod-catalog;
        in-out property <bool> default-mod-catalog-is-custom;
        in-out property <string> persistent-root;
        in-out property <string> default-persistent-root;
        in-out property <bool> persistent-root-is-custom;
        in-out property <[string]> watched-download-folders;
        in-out property <[string]> artifact-cache-limit-labels;
        in-out property <int> artifact-cache-limit-index;
        // Which entry in the list above is Custom. Held rather than hardcoded so
        // adding a preset cannot silently move the size box onto another entry.
        in-out property <int> artifact-cache-custom-index: -1;
        in-out property <[string]> artifact-cache-units;
        in-out property <string> artifact-cache-custom-value;
        in-out property <string> artifact-cache-custom-unit;
        in-out property <string> artifact-cache-used;
        in-out property <string> artifact-cache-total;
        // False until the cache has actually been measured, so the usage line
        // shows that it is loading rather than claiming the cache is empty.
        in-out property <bool> artifact-cache-usage-known;
        in-out property <bool> artifact-cache-usage-exact: true;
        in-out property <bool> artifact-cache-over-budget;
        // Guards the clear button against a second click while the first is
        // still in the backend.
        in-out property <bool> artifact-cache-clearing;
        in-out property <int> application-text-scale: 100;
        in-out property <int> technical-text-size: 12;
        in-out property <string> java-memory-min: "512";
        in-out property <string> java-memory-max: "4096";
        in-out property <string> java-preferred-path;
        // What one instance pins for itself. Each field is empty while the
        // instance follows the global setting, which is a different state from
        // pinning the value the global one currently holds.
        in-out property <string> instance-java-path;
        in-out property <string> instance-memory-min;
        in-out property <string> instance-memory-max;
        in-out property <string> instance-jvm-args;
        in-out property <bool> instance-runtime-pinned: false;
        in-out property <string> instance-runtime-global;
        in-out property <bool> java-auto-detect: true;
        in-out property <bool> java-auto-download: true;
        in-out property <bool> show-linked-servers: false;
        in-out property <bool> make-default-account-operator: true;
        in-out property <int> operator-permission-level: 4;
        in-out property <string> server-workspace-return-client;

        callback refresh();
        callback request-version-catalog();
        callback warm-language-catalog(string);
        callback request-loader-catalog(string, string, string);
        callback select-instance(string);
        callback select-server(string);
        callback minecraft-selection-changed(string, string);
        callback toggle-favorite-version(string);
        callback remove-favorite-version(string);
        callback launch-client(string);
        // Launches one instance as a named account, for this launch only.
        // Launches one instance as a named account, for this launch only. The
        // last argument asks for a second client alongside one already running.
        callback launch-client-as(string, string, bool);
        // The instance, and which of its launches to stop. An instance can hold
        // more than one, and stopping should reach only the one asked for.
        callback kill-instance(string, string);
        callback launch-and-connect(string, string);
        callback open-client-folder(string);
        callback open-server-folder(string);
        callback duplicate-client(string);
        callback delete-client(string, bool);
        callback delete-server(string, bool);
        callback repair-server(string);
        callback save-instance-runtime(string, string, string, string, string);
        callback clear-instance-runtime(string);
        callback load-instance-workspace(string);
        callback search-mods(string, string, string);
        callback load-more-mods(string, string, string, int);
        callback switch-mod-catalog(string);
        callback install-from-browser(string, string, string);
        // The user closed a browser install's confirmation. The queue behind it
        // has to hear that, or the next queued install never starts.
        callback abandon-browser-install();
        callback set-mod-page-size(string);
        // Bumped to put the selected instance's card in view. A counter rather
        // than a function, because the list only exists while there is one to
        // show and a name inside a conditional cannot be reached from here.
        in-out property <int> reveal-selected-instance: 0;
        // The instance still waiting to be scrolled to, if any. An id rather
        // than a flag: a card's position is only known once the list holding it
        // has arrived, and by then the request has to still name what it was
        // for.
        in-out property <string> pending-reveal-instance;
        callback install-mod(string, string, string, string);
        callback submit-create-instance(string, string, string, string, bool);
        callback open-create-instance-modal();
        callback recompute-create-name();
        callback recompute-create-server-name();
        callback open-import-modal();
        callback close-import-modal();
        callback choose-import-file();
        callback toggle-import-optional(string);
        callback skip-blocked-file(string);
        callback open-blocked-file(string);
        callback import-name-edited(string);
        // No archive path: submission always uses whichever archive the
        // current analysis actually applies to, not this text.
        callback submit-import(string, bool, bool, string);
        callback submit-create-server(string, string, string, string, string, bool, bool, string, string);
        callback choose-server-pack();
        callback clear-server-pack();
        callback choose-import-server-pack();
        callback clear-import-server-pack();
        callback server-start(string);
        callback server-stop(string);
        callback server-restart(string);
        callback server-kill(string);
        callback server-console(string, string);
        callback open-live-log(string, string);
        // Reads one saved log file into the same filterable view the live log uses.
        callback open-log-file(string, string, string);
        callback close-live-log();
        callback copy-live-log();
        callback set-log-level(string, bool);
        // Moves one end of the console's selection to a character column on a
        // row, extending from the existing anchor when asked.
        callback select-log-at(int, int, bool);
        // Selects the whole word under a double click.
        callback select-log-word-at(int, int);
        callback copy-log-selection();
        callback select-all-log();
        callback search-live-log(string);
        callback live-log-follow-changed(bool);
        callback load-server-workspace(string);
        callback load-server-mods(string);
        callback load-running();
        // The instance, and which of its launches to end.
        callback force-stop-launch(string, string);
        // Asks a provider about a host's mods again. Names and artwork are
        // otherwise fetched once per host per run and repainted from the
        // shared cache after that.
        callback refresh-mods(string, string);
        callback disable-suggested-mods(string);
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
        callback choose-default-mod-catalog(string);
        callback save-curseforge-api-key(string);
        callback remove-curseforge-api-key();
        callback open-curseforge-key-help();
        callback save-storage-settings(string);
        callback choose-storage-root();
        callback reset-storage-root();
        callback choose-watched-folder();
        callback remove-watched-folder(string);
        callback measure-artifact-cache();
        callback save-artifact-cache-limit(int, string, string);
        callback clear-artifact-cache();
        callback save-display-settings(int, int);
        callback save-linked-server-settings(bool, bool, int);
        callback refresh-java();
        callback save-java-settings(bool, bool, string, string, string);
        callback minecraft-select-category(int);
        callback minecraft-set-query(string);
        callback minecraft-set-show-advanced(bool);
        callback minecraft-set-only-overridden(bool);
        callback minecraft-set-preview-release(string);
        callback minecraft-set-boolean(string, bool);
        callback minecraft-set-number(string, float);
        callback minecraft-edit-number(string, string);
        callback minecraft-commit-number(string, string);
        callback minecraft-set-enumeration(string, int);
        callback minecraft-set-language(string, int);
        callback minecraft-edit-window(string, string, string);
        callback minecraft-commit-window-width(string, string, string);
        callback minecraft-commit-window-height(string, string, string);
        callback minecraft-set-window-aspect(string, int);
        callback minecraft-set-window-lock(string, bool);
        callback minecraft-open-window-preview(string);
        callback minecraft-commit-text(string, string);
        callback minecraft-forget-preference(string);
        callback minecraft-begin-capture(string);
        callback minecraft-cancel-capture();
        callback minecraft-captured-key(string, string);
        callback minecraft-captured-mouse(string, string);
        callback minecraft-retry-languages();
        callback minecraft-save-defaults();
        callback minecraft-discard-defaults();
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
                    // No `alignment`: under anything but the default, every cell
                    // is given its preferred size, which collapses the spacer
                    // below to nothing and centers the whole bar as one block.
                    cross-axis-alignment: center;
                    padding-left: 20px; padding-right: 20px; spacing: 14px;
                    Image { source: root.logo; width: 32px; height: 32px; image-fit: contain; accessible-label: GuiI18n.text("gui-logo-accessible"); }
                    Heading { text: "Ephervex"; font-size: Theme.brand-size; }
                    Rectangle { horizontal-stretch: 1; }
                    if root.page == 0 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-import-modpack"); clicked => { root.open-import-modal(); } }
                    if root.page == 0 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-new-instance"); tone: "primary"; min-width: 148px; clicked => { root.open-create-instance-modal(); } }
                    if root.page == 1 && root.workspace == "" : AppButton { text: GuiI18n.text("gui-new-server"); tone: "primary"; min-width: 130px; clicked => { root.create-server-client-id = ""; root.create-server-name = ""; root.recompute-create-server-name(); root.modal-error = ""; root.request-version-catalog(); root.warm-language-catalog(root.create-server-minecraft); root.create-server-pack = ""; root.create-server-pack-name = ""; root.modal = "create-server"; } }
                    IconButton { icon: @image-url("../../../assets/icons/refresh.svg"); accessible-name: GuiI18n.text("gui-refresh"); clicked => { root.refresh(); } }
                }
            }

            HorizontalLayout {
                spacing: 0px;
                // Primary navigation.
                Rectangle {
                    width: Theme.sidebar-width;
                    background: #11171a;
                    border-width: 1px;
                    border-color: Theme.border;
                    VerticalLayout {
                        padding: 14px; spacing: 8px;
                        NavItem { icon: @image-url("../../../assets/icons/nav-instances.svg"); label: GuiI18n.text("gui-instances"); selected: root.page == 0; clicked => { root.close-live-log(); root.page = 0; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-servers.svg"); label: GuiI18n.text("gui-servers"); selected: root.page == 1; clicked => { root.close-live-log(); root.page = 1; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-logs.svg"); label: GuiI18n.text("gui-running"); selected: root.page == 2; clicked => { root.close-live-log(); root.page = 2; root.workspace = ""; root.load-running(); } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-activity.svg"); label: GuiI18n.text("gui-activity"); selected: root.page == 3; clicked => { root.close-live-log(); root.page = 3; root.workspace = ""; } }
                        NavItem { icon: @image-url("../../../assets/icons/nav-settings.svg"); label: GuiI18n.text("gui-settings"); selected: root.page == 4; clicked => { root.close-live-log(); root.page = 4; root.workspace = ""; root.workspace-tab = "General"; } }
                        Rectangle { vertical-stretch: 1; }
                        BodyText { text: GuiI18n.text("gui-tagline"); color: Theme.text-muted; font-size: Theme.caption-size; wrap: word-wrap; }
                    }
                }

                // One container for every page body, always present and
                // always claiming the space beside the sidebar.
                //
                // A conditional child contributes no layout information of
                // its own, so a page that asked to stretch was given only
                // the width its content preferred. That is why an instance
                // workspace collapsed into a column whenever the open tab
                // happened to be a narrow one.
                Rectangle {
                    horizontal-stretch: 1;
                    background: Theme.background;

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
                                    height: Theme.control-height + 20px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    HorizontalLayout {
                                        cross-axis-alignment: center;
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
                                    primary => { root.open-create-instance-modal(); }
                                    secondary => { root.open-import-modal(); }
                                }
                                if !root.loading && (root.instances.length > 0 || root.pending-instances.length > 0) : instance-scroll := ScrollView {
                                    viewport-width: self.width;
                                    // Cards and the gap between them are fixed,
                                    // so where one sits is arithmetic rather
                                    // than a query into the list.
                                    private property <int> observed-reveal: root.reveal-selected-instance;
                                    changed observed-reveal => {
                                        if root.selected-instance-index >= 0 {
                                            instance-scroll.viewport-y = max(
                                                min(0px, instance-scroll.visible-height - instance-scroll.viewport-height),
                                                -1 * (root.pending-instances.length + root.selected-instance-index) * (Theme.card-height + Theme.card-gap));
                                        }
                                    }
                                    VerticalLayout {
                                        spacing: Theme.card-gap;
                                        for pending in root.pending-instances : PendingCard { item: pending; }
                                        for item[index] in root.instances : InstanceCard {
                                            visible: root.instance-filter == "all" || item.storage-kind == root.instance-filter;
                                            item: item;
                                            selected: item.id == root.selected-instance-id;
                                            choose(id) => { root.selected-instance-id = id; root.select-instance(id); }
                                            launch(id) => { root.launch-client(id); }
                                            kill(id) => { root.kill-instance(id, ""); }
                                            open-workspace(id) => { root.selected-instance-id = id; root.selected-instance-index = index; root.select-instance(id); root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); }
                                            open-folder(id) => { root.open-client-folder(id); }
                                            create-server(id, name) => { root.create-server-client-id = id; root.create-server-name = ""; root.create-server-name-suggestion = name + " Server"; root.create-server-pack = ""; root.create-server-pack-name = ""; root.modal = "create-server"; }
                                            duplicate(id) => { root.duplicate-client(id); }
                                            delete(id, name) => { root.pending-action-id = id; root.pending-action-name = name; root.pending-action-linked-servers = item.linked-servers; root.modal = "delete-instance"; }
                                        }
                                    }
                                }
                            }
                        }
                        if root.show-inspector && root.selected-instance-index >= 0 : InstanceInspector {
                            width: 360px;
                            has-selection: root.selected-instance-index >= 0;
                            item: root.selected-instance-index >= 0 ? root.instances[root.selected-instance-index] : {};
                            accounts: root.accounts;
                            launch(id) => { root.launch-client(id); }
                            launch-as(id, account, alongside) => { root.launch-client-as(id, account, alongside); }
                            add-account => { root.page = 4; root.workspace = ""; root.workspace-tab = "Accounts"; }
                            kill(id) => { root.kill-instance(id, ""); }
                            connect(client, server) => { root.launch-and-connect(client, server); }
                            open-folder(id) => { root.open-client-folder(id); }
                            workspace(id) => { root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); }
                            create-server(id, name) => { root.create-server-client-id = id; root.create-server-name = ""; root.create-server-name-suggestion = name + " Server"; root.create-server-pack = ""; root.create-server-pack-name = ""; root.modal = "create-server"; }
                            diagnostics(id) => { root.workspace = "instance"; root.workspace-tab = "Logs"; root.selected-diagnostic-kind = "log"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.load-instance-workspace(id); root.open-live-log("client", id); }
                        }
                    }

                    if root.page == 0 && root.workspace == "instance" : Rectangle {
                        horizontal-stretch: 1; background: Theme.background;
                        VerticalLayout {
                            padding: 22px; spacing: 14px;
                            HorizontalLayout { spacing: 12px; AppButton { text: GuiI18n.text("gui-back-instances"); clicked => { root.close-live-log(); root.workspace = ""; } } Heading { text: root.selected-instance-index >= 0 ? root.instances[root.selected-instance-index].name : GuiI18n.text("gui-instance"); font-size: Theme.workspace-title-size; } Rectangle { horizontal-stretch: 1; } LaunchAsButton { accounts: root.accounts; enabled: root.selected-instance-index >= 0 && (root.instances[root.selected-instance-index].can-launch || root.instances[root.selected-instance-index].can-kill); alongside: root.selected-instance-index >= 0 && root.instances[root.selected-instance-index].can-kill; launch-as(account, alongside) => { root.launch-client-as(root.selected-instance-id, account, alongside); } add-account => { root.page = 4; root.workspace = ""; root.workspace-tab = "Accounts"; } } AppButton { text: GuiI18n.text("gui-launch"); tone: "primary"; enabled: root.selected-instance-index >= 0 && root.instances[root.selected-instance-index].can-launch; clicked => { root.launch-client(root.selected-instance-id); } } }
                            HorizontalLayout {
                                spacing: 4px;
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-overview"); selected: root.workspace-tab == "Overview"; clicked => { root.close-live-log(); root.workspace-tab = "Overview"; } }
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-mods"); selected: root.workspace-tab == "Mods"; clicked => { root.close-live-log(); root.workspace-tab = "Mods"; } }
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-settings"); selected: root.workspace-tab == "Settings"; clicked => { root.close-live-log(); root.workspace-tab = "Settings"; } }
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-logs"); selected: root.workspace-tab == "Logs"; clicked => { root.workspace-tab = "Logs"; root.selected-diagnostic-kind = "log"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.open-live-log("client", root.selected-instance-id); } }
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
                                    // The instance's own files, offered where the server
                                    // workspace offers a server's: the card above prints
                                    // the path, so the workspace would otherwise name a
                                    // folder it cannot open. It shares the row the loader
                                    // summary already leaves half empty rather than taking
                                    // one of its own, and the row itself is unconditional
                                    // so a vanilla instance, which has no build to change,
                                    // still has a way into its folder.
                                    HorizontalLayout { spacing: 10px;
                                        if root.instances[root.selected-instance-index].loader-kind != "vanilla" : BodyText { text: GuiI18n.value("gui-loader-change-summary", root.instances[root.selected-instance-index].loader); color: Theme.text-secondary; vertical-alignment: center; }
                                        Rectangle { horizontal-stretch: 1; }
                                        AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-client-folder(root.selected-instance-id); } }
                                        if root.instances[root.selected-instance-index].loader-kind != "vanilla" : AppButton { text: GuiI18n.text("gui-loader-change-open"); clicked => { root.open-loader-change(); } }
                                    }
                                    BodyText { text: root.instances[root.selected-instance-index].storage-kind == "temporary" ? GuiI18n.text("gui-temporary-lifecycle") : GuiI18n.text("gui-persistent-lifecycle"); color: Theme.text-muted; wrap: word-wrap; }
                                    Divider { }
                                    HorizontalLayout { Heading { text: GuiI18n.text("gui-linked-server"); font-size: Theme.section-size; } Rectangle { horizontal-stretch: 1; } if root.linked-servers.length == 0 && root.linked-pending-servers.length == 0 : AppButton { text: GuiI18n.text("gui-create-linked-server"); clicked => { root.create-server-client-id = root.selected-instance-id; root.create-server-name = ""; root.create-server-name-suggestion = root.instances[root.selected-instance-index].name + " Server"; root.create-server-pack = ""; root.create-server-pack-name = ""; root.modal = "create-server"; } } }
                                    if root.linked-servers.length == 0 && root.linked-pending-servers.length == 0 : BodyText { text: GuiI18n.text("gui-no-linked-server-detail"); color: Theme.text-muted; wrap: word-wrap; }
                                    for pending in root.linked-pending-servers : PendingCard { visible: pending.linked-client-id == root.selected-instance-id; item: pending; }
                                    for server in root.linked-servers : Rectangle { height: 142px; border-radius: 8px; border-width: 1px; border-color: server.needs-attention ? Theme.warning : Theme.border; background: Theme.surface; VerticalLayout { padding: 14px; spacing: 8px;
                                        HorizontalLayout { Heading { text: server.name; font-size: Theme.row-heading-size; } Pill { text: server.state; tone-color: server.state-kind == "running" ? Theme.success : server.needs-attention ? Theme.warning : Theme.text-secondary; } Rectangle { horizontal-stretch: 1; } BodyText { text: server.address; color: Theme.text-muted; } }
                                        BodyText { text: server.needs-attention ? GuiI18n.text("gui-server-needs-attention") : server.state-detail; color: server.needs-attention ? Theme.warning : Theme.text-secondary; }
                                        HorizontalLayout { spacing: 8px;
                                            AppButton { text: GuiI18n.text("gui-open-server-workspace"); clicked => { root.selected-server-id = server.id; root.server-workspace-return-client = root.selected-instance-id; root.page = 1; root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(server.id); } }
                                            if server.can-start || server.can-stop : AppButton { text: server.can-start ? GuiI18n.text("gui-start") : GuiI18n.text("gui-stop"); tone: "primary"; clicked => { if server.can-start { root.server-start(server.id); } else { root.server-stop(server.id); } } }
                                            AppButton { text: GuiI18n.text("gui-launch-client-connect"); enabled: server.can-connect; clicked => { root.launch-and-connect(root.selected-instance-id, server.id); } }
                                            AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(server.id); } }
                                            if server.state-kind == "needs-recovery" : AppButton { text: GuiI18n.text("gui-repair-server"); tone: "primary"; clicked => { root.repair-server(server.id); } }
                                            Rectangle { horizontal-stretch: 1; }
                                            AppButton { text: GuiI18n.text("gui-delete-server"); tone: "danger"; clicked => { root.pending-action-id = server.id; root.pending-action-name = server.name; root.modal = "delete-server"; } }
                                        }
                                    } }
                                }
                            }
                            if root.workspace-tab == "Mods" : HorizontalLayout {
                                spacing: 16px;
                                Rectangle { width: 46%; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    VerticalLayout { padding: 16px; spacing: 10px; HorizontalLayout { spacing: 8px; cross-axis-alignment: center; Heading { text: GuiI18n.text("gui-managed-mods"); horizontal-stretch: 1; } TextField { text <=> root.managed-mods-query; placeholder: GuiI18n.text("gui-filter-mods"); accessible-label: GuiI18n.text("gui-filter-mods"); input-type: InputType.search; width: 200px; edited(text) => { root.filter-mods("client", text); } } IconButton { icon: @image-url("../../../assets/icons/refresh.svg"); accessible-name: GuiI18n.text("gui-refresh-mod-details"); clicked => { root.refresh-mods("client", root.selected-instance-id); } } } BodyText { text: GuiI18n.text("gui-managed-mods-detail"); wrap: word-wrap; }
                                        if root.managed-mods-notice != "" : BodyText { text: root.managed-mods-notice; color: Theme.text-muted; font-size: Theme.caption-size; wrap: word-wrap; }
                                        if root.managed-mods.length == 0 : EmptyState { icon: @image-url("../../../assets/icons/semantic-mod.svg"); title: root.managed-mods-query != "" ? GuiI18n.text("gui-no-mods-match-title") : GuiI18n.text("gui-no-managed-mods"); message: root.managed-mods-query != "" ? GuiI18n.value("gui-no-mods-match", root.managed-mods-query) : GuiI18n.text("gui-no-managed-mods-detail"); }
                                        ScrollView { viewport-width: self.width; VerticalLayout { spacing: 8px; for mod in root.managed-mods : ModCard { item: mod; manage(path) => { root.open-mod-manager(path); } } } }
                                    }
                                }
                                Rectangle { horizontal-stretch: 1; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    VerticalLayout { padding: 16px; spacing: 10px; Heading { text: GuiI18n.text("gui-add-mods"); } BodyText { text: GuiI18n.text("gui-catalog-compatibility"); wrap: word-wrap; }
                                        HorizontalLayout { spacing: 8px; cross-axis-alignment: center; Text { text: GuiI18n.text("gui-mod-catalog"); color: Theme.text-secondary; font-size: Theme.label-size; vertical-alignment: center; }
                                            FilterChip { text: GuiI18n.text("gui-provider-modrinth"); accessible-name: GuiI18n.text("gui-provider-modrinth"); selected: root.mod-provider == "modrinth"; clicked => { root.switch-mod-catalog("modrinth"); } }
                                            FilterChip { text: GuiI18n.text("gui-provider-curseforge"); accessible-name: GuiI18n.text("gui-provider-curseforge"); selected: root.mod-provider == "curseforge"; clicked => { root.switch-mod-catalog("curseforge"); } }
                                            Rectangle { horizontal-stretch: 1; }
                                            Text { text: GuiI18n.text("gui-mod-results-per-page"); color: Theme.text-secondary; font-size: Theme.label-size; vertical-alignment: center; }
                                            page-size-box := ComboBox { width: 92px; accessible-label: GuiI18n.text("gui-mod-results-per-page"); model: root.mod-page-sizes; current-value: root.mod-page-size-label; selected(value) => { root.set-mod-page-size(value); } }
                                        }
                                        ActionRow { TextField { text <=> root.mod-query; placeholder: GuiI18n.text("gui-search-compatible-mods"); input-type: InputType.search; horizontal-stretch: 1; accepted => { root.search-mods(root.selected-instance-id, root.mod-provider, root.mod-query); } } AppButton { text: GuiI18n.text("gui-search"); tone: "primary"; enabled: !root.mod-search-loading; clicked => { root.search-mods(root.selected-instance-id, root.mod-provider, root.mod-query); } } }
                                        if root.mod-search-notice != "" : Rectangle { border-radius: 6px; border-width: 1px; border-color: Theme.danger; background: Theme.background; VerticalLayout { padding: 10px; spacing: 6px; BodyText { text: root.mod-search-notice; color: Theme.text-secondary; wrap: word-wrap; } if root.mod-provider == "curseforge" : HorizontalLayout { AppButton { text: GuiI18n.text("gui-open-providers-settings"); clicked => { root.page = 4; root.workspace = ""; root.workspace-tab = "Providers & APIs"; } } Rectangle { horizontal-stretch: 1; } } } }
                                        if root.mod-search-loading : BodyText { text: GuiI18n.text("gui-mod-search-running"); color: Theme.text-muted; }
                                        if !root.mod-search-loading && root.mod-search-ran && root.mod-search-notice == "" && root.mod-results.length == 0 && root.mod-results-next-offset < 0 : EmptyState { icon: @image-url("../../../assets/icons/semantic-mod.svg"); title: GuiI18n.text("gui-no-mod-results"); message: GuiI18n.text("gui-no-mod-results-detail"); }
                                        ScrollView { VerticalLayout { spacing: 8px; for mod in root.mod-results : Rectangle { height: 92px; clip: true; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; HorizontalLayout { padding: 10px; spacing: 11px; VerticalLayout { alignment: center; ModArtwork { icon: mod.icon; origin-kind: "user"; } } VerticalLayout { horizontal-stretch: 1; Heading { text: mod.title; font-size: Theme.row-heading-size; overflow: elide; } BodyText { text: mod.description; wrap: word-wrap; overflow: elide; vertical-alignment: top; height: Theme.body-size * 2.7; } BodyText { text: GuiI18n.values("gui-mod-result-meta", mod.author, mod.note); color: Theme.text-muted; overflow: elide; } } VerticalLayout { alignment: center; AppButton { width: 120px; text: mod.installed ? GuiI18n.text("gui-mod-result-installed") : mod.queued ? GuiI18n.text("gui-mod-result-queued") : GuiI18n.text("gui-install"); tone: mod.installed || mod.queued ? "secondary" : "primary"; enabled: !mod.installed && !mod.queued; clicked => { root.install-from-browser(root.selected-instance-id, root.mod-provider, mod.project-id); } } } } }
                                            if root.mod-results-next-offset >= 0 : HorizontalLayout { alignment: center; AppButton { text: GuiI18n.text("gui-load-more"); enabled: !root.mod-search-loading; clicked => { root.load-more-mods(root.selected-instance-id, root.mod-search-provider, root.mod-search-submitted, root.mod-results-next-offset); } } }
                                        } }
                                    }
                                }
                            }
                            if root.workspace-tab == "Settings" : ScrollView { viewport-width: self.width; VerticalLayout { alignment: start; width: min(parent.width, 720px); spacing: 12px;
                                SectionTitle { title: GuiI18n.text("gui-instance-runtime"); detail: GuiI18n.text("gui-instance-runtime-detail"); }
                                BodyText { text: root.instance-runtime-global; color: Theme.text-muted; wrap: word-wrap; }
                                Text { text: GuiI18n.text("gui-java-preferred"); color: Theme.text-secondary; font-size: Theme.label-size; }
                                TextField { text <=> root.instance-java-path; placeholder: GuiI18n.text("gui-instance-runtime-inherited"); accessible-name: GuiI18n.text("gui-java-preferred"); }
                                HorizontalLayout { spacing: 12px;
                                    VerticalLayout { spacing: 4px; FieldLabel { text: GuiI18n.text("gui-java-memory-min"); } TextField { text <=> root.instance-memory-min; placeholder: GuiI18n.text("gui-instance-runtime-inherited"); accessible-name: GuiI18n.text("gui-java-memory-min"); input-type: InputType.number; width: 190px; } }
                                    VerticalLayout { spacing: 4px; FieldLabel { text: GuiI18n.text("gui-java-memory-max"); } TextField { text <=> root.instance-memory-max; placeholder: GuiI18n.text("gui-instance-runtime-inherited"); accessible-name: GuiI18n.text("gui-java-memory-max"); input-type: InputType.number; width: 190px; } }
                                    Rectangle { horizontal-stretch: 1; }
                                }
                                Text { text: GuiI18n.text("gui-instance-jvm-args"); color: Theme.text-secondary; font-size: Theme.label-size; }
                                TextField { text <=> root.instance-jvm-args; placeholder: GuiI18n.text("gui-instance-jvm-args-placeholder"); accessible-name: GuiI18n.text("gui-instance-jvm-args"); }
                                if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; }
                                HorizontalLayout { spacing: 8px;
                                    if root.instance-runtime-pinned : AppButton { text: GuiI18n.text("gui-instance-runtime-clear"); clicked => { root.clear-instance-runtime(root.selected-instance-id); } }
                                    Rectangle { horizontal-stretch: 1; }
                                    AppButton { text: GuiI18n.text("gui-instance-runtime-save"); tone: "primary"; clicked => { root.save-instance-runtime(root.selected-instance-id, root.instance-java-path, root.instance-memory-min, root.instance-memory-max, root.instance-jvm-args); } }
                                }
                                Divider { }
                                SectionTitle { title: GuiI18n.text("gui-instance-settings-unavailable"); detail: GuiI18n.text("gui-instance-settings-unavailable-detail"); }
                                HorizontalLayout { AppButton { text: GuiI18n.text("gui-open-global-defaults"); clicked => { root.page = 4; root.workspace = ""; root.workspace-tab = "Minecraft"; } } Rectangle { horizontal-stretch: 1; } }
                            } }
                            if root.workspace-tab == "Logs" : VerticalLayout { spacing: 9px;
                                HorizontalLayout { spacing: 8px;
                                    FilterChip { text: GuiI18n.text("gui-live-latest-log"); accessible-name: GuiI18n.text("gui-live-latest-log"); selected: root.selected-diagnostic-kind != "crash-report" && !root.log-shows-files; clicked => { root.selected-diagnostic-kind = "log"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.open-live-log("client", root.selected-instance-id); } }
                                    FilterChip { text: GuiI18n.text("gui-log-files"); accessible-name: GuiI18n.text("gui-log-files"); selected: root.selected-diagnostic-kind != "crash-report" && root.log-shows-files; clicked => { root.close-live-log(); root.selected-diagnostic-kind = "log"; root.log-shows-files = true; root.selected-diagnostic-name = ""; root.load-diagnostics("client", root.selected-instance-id, "log"); } }
                                    FilterChip { text: GuiI18n.text("gui-crash-reports"); accessible-name: GuiI18n.text("gui-crash-reports"); selected: root.selected-diagnostic-kind == "crash-report"; clicked => { root.close-live-log(); root.selected-diagnostic-kind = "crash-report"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.load-diagnostics("client", root.selected-instance-id, "crash-report"); } }
                                    Rectangle { horizontal-stretch: 1; }
                                    AppButton { text: GuiI18n.text("gui-folder"); clicked => { root.open-diagnostic-folder("client", root.selected-instance-id, root.selected-diagnostic-kind); } }
                                }
                                // The log being written to, followed as it grows.
                                if root.selected-diagnostic-kind != "crash-report" && !root.log-shows-files : LiveLogViewer {
                                    chunks: root.log-chunks; has-content: root.log-has-content; status: root.log-status; history-truncated: root.log-history-truncated; pending-lines: root.log-pending-lines; content-columns: root.log-content-columns; font-size: root.technical-text-size * 1px; query <=> root.log-query; wrap-lines <=> root.log-wrap-lines; follow <=> root.log-follow; trace-enabled: root.log-trace-enabled; debug-enabled: root.log-debug-enabled; info-enabled: root.log-info-enabled; warn-enabled: root.log-warn-enabled; error-enabled: root.log-error-enabled; fatal-enabled: root.log-fatal-enabled; other-enabled: root.log-other-enabled; revision: root.log-revision; selection-start-row: root.log-selection-start-row; selection-start-offset: root.log-selection-start-offset; selection-end-row: root.log-selection-end-row; selection-end-offset: root.log-selection-end-offset; has-selection: root.log-has-selection; selection-revision: root.log-selection-revision;
                                    set-level(level, enabled) => { root.set-log-level(level, enabled); } search(query) => { root.search-live-log(query); } copy => { root.copy-live-log(); } copy-selection => { root.copy-log-selection(); } select-at(row, column, extend) => { root.select-log-at(row, column, extend); } select-word-at(row, column) => { root.select-log-word-at(row, column); } select-all => { root.select-all-log(); } follow-changed(follow) => { root.live-log-follow-changed(follow); }
                                }
                                // A saved log, read through the same view: a
                                // finished log has the same levels as a live
                                // one and is read for the same reasons.
                                if root.selected-diagnostic-kind != "crash-report" && root.log-shows-files : HorizontalLayout { spacing: 10px;
                                    DiagnosticFileList { items: root.diagnostics; selected: root.selected-diagnostic-name; opened(name) => { root.selected-diagnostic-name = name; root.open-log-file("client", root.selected-instance-id, name); } }
                                    if root.selected-diagnostic-name == "" : Rectangle { horizontal-stretch: 1; background: rgb(11, 14, 16); border-radius: 8px; border-width: 1px; border-color: Theme.border; EmptyState { icon: @image-url("../../../assets/icons/nav-logs.svg"); title: GuiI18n.text("gui-no-file-open"); message: GuiI18n.text("gui-no-file-open-detail"); } }
                                    if root.selected-diagnostic-name != "" : LiveLogViewer {
                                    chunks: root.log-chunks; has-content: root.log-has-content; status: root.log-status; history-truncated: root.log-history-truncated; pending-lines: root.log-pending-lines; content-columns: root.log-content-columns; font-size: root.technical-text-size * 1px; query <=> root.log-query; wrap-lines <=> root.log-wrap-lines; follow <=> root.log-follow; trace-enabled: root.log-trace-enabled; debug-enabled: root.log-debug-enabled; info-enabled: root.log-info-enabled; warn-enabled: root.log-warn-enabled; error-enabled: root.log-error-enabled; fatal-enabled: root.log-fatal-enabled; other-enabled: root.log-other-enabled; revision: root.log-revision; selection-start-row: root.log-selection-start-row; selection-start-offset: root.log-selection-start-offset; selection-end-row: root.log-selection-end-row; selection-end-offset: root.log-selection-end-offset; has-selection: root.log-has-selection; selection-revision: root.log-selection-revision;
                                    set-level(level, enabled) => { root.set-log-level(level, enabled); } search(query) => { root.search-live-log(query); } copy => { root.copy-live-log(); } copy-selection => { root.copy-log-selection(); } select-at(row, column, extend) => { root.select-log-at(row, column, extend); } select-word-at(row, column) => { root.select-log-word-at(row, column); } select-all => { root.select-all-log(); } follow-changed(follow) => { root.live-log-follow-changed(follow); }
                                }
                                }
                                // A crash report stays a plain document. It is
                                // one stack dump, and a level filter over it
                                // would hide the report rather than narrow it.
                                if root.selected-diagnostic-kind == "crash-report" : VerticalLayout { spacing: 8px;
                                    HorizontalLayout { spacing: 8px; Rectangle { horizontal-stretch: 1; } TextField { text <=> root.diagnostic-query; placeholder: GuiI18n.text("gui-search-loaded-content"); input-type: InputType.search; width: 250px; accepted => { root.search-diagnostic("client", root.selected-instance-id, "crash-report", root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-find"); enabled: root.selected-diagnostic-name != ""; clicked => { root.search-diagnostic("client", root.selected-instance-id, "crash-report", root.selected-diagnostic-name, root.diagnostic-query); } } }
                                    HorizontalLayout { spacing: 10px;
                                        DiagnosticFileList { items: root.diagnostics; selected: root.selected-diagnostic-name; crash: true; opened(name) => { root.selected-diagnostic-name = name; root.open-diagnostic("client", root.selected-instance-id, "crash-report", name); } }
                                        Rectangle { horizontal-stretch: 1; background: rgb(11, 14, 16); border-radius: 8px; border-width: 1px; border-color: Theme.border; VerticalLayout { if root.diagnostic-content == "" : EmptyState { icon: @image-url("../../../assets/icons/nav-logs.svg"); title: GuiI18n.text("gui-no-file-open"); message: GuiI18n.text("gui-no-file-open-detail"); } if root.diagnostic-content != "" : SelectableText { text: root.diagnostic-content; font-size: root.technical-text-size * 1px; } if root.diagnostic-content != "" : AppButton { text: GuiI18n.text("gui-copy"); clicked => { root.copy-text(root.diagnostic-content); } } } }
                                    }
                                }
                            }
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
                                if root.global-servers.length == 0 && root.pending-servers.length == 0 : EmptyState { icon: @image-url("../../../assets/icons/nav-servers.svg"); title: GuiI18n.text("gui-no-servers"); message: GuiI18n.text("gui-no-standalone-servers-detail"); primary-text: GuiI18n.text("gui-modal-new-server"); primary => { root.create-server-client-id = ""; root.create-server-name = ""; root.recompute-create-server-name(); root.modal-error = ""; root.request-version-catalog(); root.warm-language-catalog(root.create-server-minecraft); root.create-server-pack = ""; root.create-server-pack-name = ""; root.modal = "create-server"; } }
                                if root.global-servers.length > 0 || root.pending-servers.length > 0 : ScrollView { viewport-width: self.width; VerticalLayout { spacing: 12px; for pending in root.pending-servers : PendingCard { visible: root.show-linked-servers || pending.linked-client-id == ""; item: pending; } for item[index] in root.global-servers : ServerCard { item: item; selected: item.id == root.selected-global-server-id; choose(id) => { root.selected-global-server-id = id; root.selected-global-server-index = index; root.select-server(id); } primary(id) => { if item.can-start { root.server-start(id); } else { root.server-stop(id); } } open-workspace(id) => { root.selected-global-server-id = id; root.selected-global-server-index = index; root.selected-server-id = id; root.server-workspace-return-client = ""; root.select-server(id); root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(id); } open-folder(id) => { root.open-server-folder(id); } restart(id) => { root.server-restart(id); } kill(id, name) => { root.selected-server-id = id; root.pending-action-name = name; root.modal = "kill"; } delete(id, name) => { root.pending-action-id = id; root.pending-action-name = name; root.modal = "delete-server"; } } } }
                            }
                        }
                        if root.show-inspector && root.selected-global-server-index >= 0 : Rectangle { width: 360px; background: Theme.surface; border-width: 1px; border-color: Theme.border;
                            if root.selected-global-server-index < 0 : EmptyState { icon: @image-url("../../../assets/icons/nav-servers.svg"); title: GuiI18n.text("gui-no-server-selected"); message: GuiI18n.text("gui-select-server"); }
                            if root.selected-global-server-index >= 0 : VerticalLayout { alignment: start; padding: 22px; spacing: 12px;
                                HorizontalLayout {
                                    spacing: 12px;
                                    LoaderGlyph { kind: root.global-servers[root.selected-global-server-index].loader-kind; server: true; }
                                    VerticalLayout { spacing: 4px;
                                        Heading { text: root.global-servers[root.selected-global-server-index].name; }
                                        Pill { text: root.global-servers[root.selected-global-server-index].state; tone-color: root.global-servers[root.selected-global-server-index].state-kind == "running" ? Theme.success : Theme.text-secondary; }
                                    }
                                }
                                Divider { }
                                Eyebrow { text: GuiI18n.text("gui-server-run"); }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-target"); } BodyText { text: root.global-servers[root.selected-global-server-index].minecraft + " | " + root.global-servers[root.selected-global-server-index].loader; } }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-relation"); } BodyText { text: root.global-servers[root.selected-global-server-index].relationship; } }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-address"); } BodyText { text: root.global-servers[root.selected-global-server-index].address; } }
                                Divider { }
                                Eyebrow { text: GuiI18n.text("gui-actions"); }
                                if root.global-servers[root.selected-global-server-index].can-start || root.global-servers[root.selected-global-server-index].can-stop : AppButton { text: root.global-servers[root.selected-global-server-index].can-start ? GuiI18n.text("gui-start") : GuiI18n.text("gui-stop"); tone: "primary"; clicked => { if root.global-servers[root.selected-global-server-index].can-start { root.server-start(root.selected-global-server-id); } else { root.server-stop(root.selected-global-server-id); } } }
                                HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-open-server-workspace"); clicked => { root.selected-server-id = root.selected-global-server-id; root.server-workspace-return-client = ""; root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.global-servers[root.selected-global-server-index].can-restart; clicked => { root.server-restart(root.selected-global-server-id); } } }
                                if root.global-servers[root.selected-global-server-index].linked-client-id != "" : AppButton { text: GuiI18n.text("gui-launch-client-connect"); enabled: root.global-servers[root.selected-global-server-index].can-connect; clicked => { root.launch-and-connect(root.global-servers[root.selected-global-server-index].linked-client-id, root.selected-global-server-id); } }
                                HorizontalLayout { spacing: 8px;
                                    AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(root.selected-global-server-id); } }
                                    AppButton { text: GuiI18n.text("gui-view-logs"); clicked => { root.selected-server-id = root.selected-global-server-id; root.server-workspace-return-client = ""; root.workspace = "server"; root.workspace-tab = "Logs"; root.selected-diagnostic-kind = "log"; root.load-server-workspace(root.selected-server-id); root.load-diagnostics("server", root.selected-server-id, "log"); } }
                                }
                                Divider { }
                                AppButton { text: GuiI18n.text("gui-force-kill"); tone: "danger"; enabled: root.global-servers[root.selected-global-server-index].can-kill; clicked => { root.selected-server-id = root.selected-global-server-id; root.modal = "kill"; } }
                            }
                        }
                    }

                    if root.page == 1 && root.workspace == "server" : Rectangle { horizontal-stretch: 1; background: Theme.background;
                        VerticalLayout { padding: 22px; spacing: 14px;
                            HorizontalLayout { spacing: 12px; AppButton { text: root.server-workspace-return-client != "" ? GuiI18n.text("gui-back-instance") : GuiI18n.text("gui-back-servers"); clicked => { root.close-live-log(); if root.server-workspace-return-client != "" { root.page = 0; root.workspace = "instance"; root.workspace-tab = "Overview"; root.server-workspace-return-client = ""; } else { root.workspace = ""; } } } Heading { text: root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : GuiI18n.text("gui-server"); font-size: Theme.workspace-title-size; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: root.selected-server-index >= 0 && root.servers[root.selected-server-index].can-stop ? GuiI18n.text("gui-stop") : GuiI18n.text("gui-start"); tone: "primary"; enabled: root.selected-server-index >= 0 && (root.servers[root.selected-server-index].can-start || root.servers[root.selected-server-index].can-stop); clicked => { if root.servers[root.selected-server-index].can-stop { root.server-stop(root.selected-server-id); } else { root.server-start(root.selected-server-id); } } } }
                            HorizontalLayout { spacing: 8px;
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-overview"); selected: root.workspace-tab == "Overview"; clicked => { root.close-live-log(); root.workspace-tab = "Overview"; } }
                                WorkspaceTab { text: GuiI18n.text("gui-server-tab-console"); selected: root.workspace-tab == "Console"; clicked => { root.workspace-tab = "Console"; root.open-live-log("server", root.selected-server-id); } }
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-mods"); selected: root.workspace-tab == "Mods"; clicked => { root.close-live-log(); root.workspace-tab = "Mods"; root.load-server-mods(root.selected-server-id); } }
                                WorkspaceTab { text: GuiI18n.text("gui-server-tab-properties"); selected: root.workspace-tab == "Properties"; clicked => { root.close-live-log(); root.workspace-tab = "Properties"; } }
                                WorkspaceTab { text: GuiI18n.text("gui-instance-tab-logs"); selected: root.workspace-tab == "Logs"; clicked => { root.workspace-tab = "Logs"; root.selected-diagnostic-kind = "log"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.open-live-log("server", root.selected-server-id); } }
                                Rectangle { horizontal-stretch: 1; }
                            }
                            Divider { }
                            if root.workspace-tab == "Overview" : ScrollView { VerticalLayout { alignment: start; width: min(parent.width, 980px); spacing: 12px; Heading { text: GuiI18n.text("gui-dedicated-server"); } BodyText { text: root.servers[root.selected-server-index].state-detail; }
                                if root.crash-report != "" : CrashNotice { report: root.crash-report; failure: root.crash-failure; client-code-on-server: root.crash-client-code-on-server; suggested: root.crash-suggested; review => { root.close-live-log(); root.workspace-tab = "Mods"; root.load-server-mods(root.selected-server-id); } } Rectangle { height: 160px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 18px; spacing: 7px;
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-exact-target"); } BodyText { text: GuiI18n.values("gui-target-summary", root.servers[root.selected-server-index].minecraft, root.servers[root.selected-server-index].loader); } }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-runtime"); } BodyText { text: root.servers[root.selected-server-index].state + " | " + root.servers[root.selected-server-index].state-detail; } }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-network"); } BodyText { text: root.servers[root.selected-server-index].address; } }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-storage"); } BodyText { text: root.servers[root.selected-server-index].storage; } }
                                HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-relationship"); } BodyText { text: root.servers[root.selected-server-index].relationship; } }
                            } } HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-open-folder"); clicked => { root.open-server-folder(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-view-logs"); clicked => { root.workspace-tab = "Logs"; root.selected-diagnostic-kind = "log"; root.load-diagnostics("server", root.selected-server-id, "log"); } } if root.servers[root.selected-server-index].linked-client-id != "" : AppButton { text: GuiI18n.text("gui-launch-client-connect"); tone: "primary"; enabled: root.servers[root.selected-server-index].can-connect; clicked => { root.launch-and-connect(root.servers[root.selected-server-index].linked-client-id, root.selected-server-id); } } if root.servers[root.selected-server-index].state-kind == "needs-recovery" : AppButton { text: GuiI18n.text("gui-repair-server"); tone: "primary"; clicked => { root.repair-server(root.selected-server-id); } } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-delete-server"); tone: "danger"; clicked => { root.pending-action-id = root.selected-server-id; root.pending-action-name = root.servers[root.selected-server-index].name; root.modal = "delete-server"; } } } } }
                            if root.workspace-tab == "Console" : VerticalLayout { spacing: 10px;
                                ActionRow { BodyText { text: GuiI18n.value("gui-bounded-output", root.servers[root.selected-server-index].state); color: root.servers[root.selected-server-index].can-send-command ? Theme.success : Theme.text-secondary; } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-restart"); enabled: root.servers[root.selected-server-index].can-restart; clicked => { root.server-restart(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-stop"); enabled: root.servers[root.selected-server-index].can-stop; clicked => { root.server-stop(root.selected-server-id); } } AppButton { text: GuiI18n.text("gui-kill"); tone: "danger"; enabled: root.servers[root.selected-server-index].can-kill; clicked => { root.modal = "kill"; } } }
                                LiveLogViewer {
                                    chunks: root.log-chunks; has-content: root.log-has-content; status: root.log-status; history-truncated: root.log-history-truncated; pending-lines: root.log-pending-lines; content-columns: root.log-content-columns; font-size: root.technical-text-size * 1px; query <=> root.log-query; wrap-lines <=> root.log-wrap-lines; follow <=> root.log-follow; trace-enabled: root.log-trace-enabled; debug-enabled: root.log-debug-enabled; info-enabled: root.log-info-enabled; warn-enabled: root.log-warn-enabled; error-enabled: root.log-error-enabled; fatal-enabled: root.log-fatal-enabled; other-enabled: root.log-other-enabled; revision: root.log-revision; selection-start-row: root.log-selection-start-row; selection-start-offset: root.log-selection-start-offset; selection-end-row: root.log-selection-end-row; selection-end-offset: root.log-selection-end-offset; has-selection: root.log-has-selection; selection-revision: root.log-selection-revision;
                                    set-level(level, enabled) => { root.set-log-level(level, enabled); } search(query) => { root.search-live-log(query); } copy => { root.copy-live-log(); } copy-selection => { root.copy-log-selection(); } select-at(row, column, extend) => { root.select-log-at(row, column, extend); } select-word-at(row, column) => { root.select-log-word-at(row, column); } select-all => { root.select-all-log(); } follow-changed(follow) => { root.live-log-follow-changed(follow); }
                                }
                                ActionRow { TextField { text <=> root.console-command; placeholder: root.servers[root.selected-server-index].can-send-command ? GuiI18n.text("gui-enter-server-command") : GuiI18n.text("gui-start-server-command"); enabled: root.servers[root.selected-server-index].can-send-command; horizontal-stretch: 1; accepted => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } AppButton { text: GuiI18n.text("gui-send"); tone: "primary"; enabled: root.servers[root.selected-server-index].can-send-command && root.console-command != ""; clicked => { root.server-console(root.selected-server-id, root.console-command); root.console-command = ""; } } }
                            }
                            if root.workspace-tab == "Logs" : VerticalLayout { spacing: 9px;
                                HorizontalLayout { spacing: 8px;
                                    FilterChip { text: GuiI18n.text("gui-live-latest-log"); accessible-name: GuiI18n.text("gui-live-latest-log"); selected: root.selected-diagnostic-kind != "crash-report" && !root.log-shows-files; clicked => { root.selected-diagnostic-kind = "log"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.open-live-log("server", root.selected-server-id); } }
                                    FilterChip { text: GuiI18n.text("gui-log-files"); accessible-name: GuiI18n.text("gui-log-files"); selected: root.selected-diagnostic-kind != "crash-report" && root.log-shows-files; clicked => { root.close-live-log(); root.selected-diagnostic-kind = "log"; root.log-shows-files = true; root.selected-diagnostic-name = ""; root.load-diagnostics("server", root.selected-server-id, "log"); } }
                                    FilterChip { text: GuiI18n.text("gui-crash-reports"); accessible-name: GuiI18n.text("gui-crash-reports"); selected: root.selected-diagnostic-kind == "crash-report"; clicked => { root.close-live-log(); root.selected-diagnostic-kind = "crash-report"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.load-diagnostics("server", root.selected-server-id, "crash-report"); } }
                                    Rectangle { horizontal-stretch: 1; }
                                    AppButton { text: GuiI18n.text("gui-folder"); clicked => { root.open-diagnostic-folder("server", root.selected-server-id, root.selected-diagnostic-kind); } }
                                }
                                // The log being written to, followed as it grows.
                                if root.selected-diagnostic-kind != "crash-report" && !root.log-shows-files : LiveLogViewer {
                                    chunks: root.log-chunks; has-content: root.log-has-content; status: root.log-status; history-truncated: root.log-history-truncated; pending-lines: root.log-pending-lines; content-columns: root.log-content-columns; font-size: root.technical-text-size * 1px; query <=> root.log-query; wrap-lines <=> root.log-wrap-lines; follow <=> root.log-follow; trace-enabled: root.log-trace-enabled; debug-enabled: root.log-debug-enabled; info-enabled: root.log-info-enabled; warn-enabled: root.log-warn-enabled; error-enabled: root.log-error-enabled; fatal-enabled: root.log-fatal-enabled; other-enabled: root.log-other-enabled; revision: root.log-revision; selection-start-row: root.log-selection-start-row; selection-start-offset: root.log-selection-start-offset; selection-end-row: root.log-selection-end-row; selection-end-offset: root.log-selection-end-offset; has-selection: root.log-has-selection; selection-revision: root.log-selection-revision;
                                    set-level(level, enabled) => { root.set-log-level(level, enabled); } search(query) => { root.search-live-log(query); } copy => { root.copy-live-log(); } copy-selection => { root.copy-log-selection(); } select-at(row, column, extend) => { root.select-log-at(row, column, extend); } select-word-at(row, column) => { root.select-log-word-at(row, column); } select-all => { root.select-all-log(); } follow-changed(follow) => { root.live-log-follow-changed(follow); }
                                }
                                // A saved log, read through the same view: a
                                // finished log has the same levels as a live
                                // one and is read for the same reasons.
                                if root.selected-diagnostic-kind != "crash-report" && root.log-shows-files : HorizontalLayout { spacing: 10px;
                                    DiagnosticFileList { items: root.diagnostics; selected: root.selected-diagnostic-name; opened(name) => { root.selected-diagnostic-name = name; root.open-log-file("server", root.selected-server-id, name); } }
                                    if root.selected-diagnostic-name == "" : Rectangle { horizontal-stretch: 1; background: rgb(11, 14, 16); border-radius: 8px; border-width: 1px; border-color: Theme.border; EmptyState { icon: @image-url("../../../assets/icons/nav-logs.svg"); title: GuiI18n.text("gui-no-file-open"); message: GuiI18n.text("gui-no-file-open-detail"); } }
                                    if root.selected-diagnostic-name != "" : LiveLogViewer {
                                    chunks: root.log-chunks; has-content: root.log-has-content; status: root.log-status; history-truncated: root.log-history-truncated; pending-lines: root.log-pending-lines; content-columns: root.log-content-columns; font-size: root.technical-text-size * 1px; query <=> root.log-query; wrap-lines <=> root.log-wrap-lines; follow <=> root.log-follow; trace-enabled: root.log-trace-enabled; debug-enabled: root.log-debug-enabled; info-enabled: root.log-info-enabled; warn-enabled: root.log-warn-enabled; error-enabled: root.log-error-enabled; fatal-enabled: root.log-fatal-enabled; other-enabled: root.log-other-enabled; revision: root.log-revision; selection-start-row: root.log-selection-start-row; selection-start-offset: root.log-selection-start-offset; selection-end-row: root.log-selection-end-row; selection-end-offset: root.log-selection-end-offset; has-selection: root.log-has-selection; selection-revision: root.log-selection-revision;
                                    set-level(level, enabled) => { root.set-log-level(level, enabled); } search(query) => { root.search-live-log(query); } copy => { root.copy-live-log(); } copy-selection => { root.copy-log-selection(); } select-at(row, column, extend) => { root.select-log-at(row, column, extend); } select-word-at(row, column) => { root.select-log-word-at(row, column); } select-all => { root.select-all-log(); } follow-changed(follow) => { root.live-log-follow-changed(follow); }
                                }
                                }
                                // A crash report stays a plain document. It is
                                // one stack dump, and a level filter over it
                                // would hide the report rather than narrow it.
                                if root.selected-diagnostic-kind == "crash-report" : VerticalLayout { spacing: 8px;
                                    HorizontalLayout { spacing: 8px; Rectangle { horizontal-stretch: 1; } TextField { text <=> root.diagnostic-query; placeholder: GuiI18n.text("gui-search-loaded-content"); input-type: InputType.search; width: 250px; accepted => { root.search-diagnostic("server", root.selected-server-id, "crash-report", root.selected-diagnostic-name, root.diagnostic-query); } } AppButton { text: GuiI18n.text("gui-find"); enabled: root.selected-diagnostic-name != ""; clicked => { root.search-diagnostic("server", root.selected-server-id, "crash-report", root.selected-diagnostic-name, root.diagnostic-query); } } }
                                    HorizontalLayout { spacing: 10px;
                                        DiagnosticFileList { items: root.diagnostics; selected: root.selected-diagnostic-name; crash: true; opened(name) => { root.selected-diagnostic-name = name; root.open-diagnostic("server", root.selected-server-id, "crash-report", name); } }
                                        Rectangle { horizontal-stretch: 1; background: rgb(11, 14, 16); border-radius: 8px; border-width: 1px; border-color: Theme.border; VerticalLayout { if root.diagnostic-content == "" : EmptyState { icon: @image-url("../../../assets/icons/nav-logs.svg"); title: GuiI18n.text("gui-no-file-open"); message: GuiI18n.text("gui-no-file-open-detail"); } if root.diagnostic-content != "" : SelectableText { text: root.diagnostic-content; font-size: root.technical-text-size * 1px; } if root.diagnostic-content != "" : AppButton { text: GuiI18n.text("gui-copy"); clicked => { root.copy-text(root.diagnostic-content); } } } }
                                    }
                                }
                            }
                            if root.workspace-tab == "Mods" : VerticalLayout { spacing: 10px;
                                HorizontalLayout { spacing: 8px; SectionTitle { title: GuiI18n.text("gui-server-mods"); detail: GuiI18n.text("gui-server-mods-detail"); horizontal-stretch: 1; } VerticalLayout { alignment: center; TextField { text <=> root.server-mods-query; placeholder: GuiI18n.text("gui-filter-mods"); accessible-label: GuiI18n.text("gui-filter-mods"); input-type: InputType.search; width: 200px; edited(text) => { root.filter-mods("server", text); } } } VerticalLayout { alignment: center; IconButton { icon: @image-url("../../../assets/icons/refresh.svg"); accessible-name: GuiI18n.text("gui-refresh-mod-details"); clicked => { root.refresh-mods("server", root.selected-server-id); } } } }
                                if root.crash-report != "" : CrashNotice { report: root.crash-report; failure: root.crash-failure; client-code-on-server: root.crash-client-code-on-server; suggested: root.crash-suggested; beside-the-mods: true; disable-suggested => { root.disable-suggested-mods(root.selected-server-id); } }
                                if root.server-mods-notice != "" : BodyText { text: root.server-mods-notice; color: Theme.text-muted; font-size: Theme.caption-size; wrap: word-wrap; }
                                if root.server-mods.length == 0 : EmptyState { icon: @image-url("../../../assets/icons/semantic-mod.svg"); title: root.server-mods-query != "" ? GuiI18n.text("gui-no-mods-match-title") : GuiI18n.text("gui-no-managed-mods"); message: root.server-mods-query != "" ? GuiI18n.value("gui-no-mods-match", root.server-mods-query) : GuiI18n.text("gui-no-server-mods-detail"); }
                                if root.server-mods.length > 0 : ScrollView { viewport-width: self.width; VerticalLayout { spacing: 8px; for mod in root.server-mods : ModCard { item: mod; togglable: true; toggle(path, on) => { root.set-mod-enabled("server", root.selected-server-id, path, on); } } } }
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
                                ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-changes"); tone: "primary"; enabled: root.servers[root.selected-server-index].state-kind == "stopped"; clicked => { root.save-server-properties(root.selected-server-id, root.property-motd, root.property-max-players, root.property-difficulty, root.property-gamemode, root.property-allow-flight, root.property-pvp, root.property-allowlist, root.property-online-mode, root.property-access, root.property-address, root.property-port); } } }
                            } }
                        }
                    }

                    // What is running, which is a different question from what
                    // the launcher is doing: an operation has a progress
                    // fraction and a Cancel, and a running game has neither.
                    if root.page == 2 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                        VerticalLayout { padding: 24px; spacing: 18px;
                            PageIntro { title: GuiI18n.text("gui-running"); subtitle: GuiI18n.text("gui-running-detail"); }
                            if root.running.length == 0 : EmptyState { icon: @image-url("../../../assets/icons/nav-activity.svg"); title: GuiI18n.text("gui-nothing-running"); message: GuiI18n.text("gui-nothing-running-detail"); }
                            ScrollView { viewport-width: self.width; VerticalLayout { spacing: 10px; for item in root.running : RunningCard {
                                item: item;
                                open-log(kind, id) => { if kind == "server" { root.selected-server-id = id; root.page = 1; root.workspace = "server"; root.workspace-tab = "Logs"; root.load-server-workspace(id); } else { root.selected-instance-id = id; root.page = 0; root.workspace = "instance"; root.workspace-tab = "Logs"; root.load-instance-workspace(id); } root.selected-diagnostic-kind = "log"; root.log-shows-files = false; root.selected-diagnostic-name = ""; root.open-live-log(kind, id); }
                                open-console(id) => { root.selected-server-id = id; root.page = 1; root.workspace = "server"; root.workspace-tab = "Console"; root.load-server-workspace(id); root.open-live-log("server", id); }
                                open-workspace(kind, id) => { if kind == "server" { root.selected-server-id = id; root.page = 1; root.workspace = "server"; root.workspace-tab = "Overview"; root.load-server-workspace(id); } else { root.selected-instance-id = id; root.page = 0; root.workspace = "instance"; root.workspace-tab = "Overview"; root.load-instance-workspace(id); } }
                                stop(kind, id) => { if kind == "server" { root.server-stop(id); } else { root.kill-instance(id, item.launch); } }
                                force-stop(id, launch) => { root.force-stop-launch(id, launch); }
                            } } }
                        }
                    }

                    if root.page == 3 : Rectangle { horizontal-stretch: 1; background: Theme.background;
                        VerticalLayout { padding: 24px; spacing: 18px;
                            PageIntro { title: GuiI18n.text("gui-activity"); subtitle: GuiI18n.text("gui-activity-detail"); }
                            if root.activities.length == 0 : EmptyState { icon: @image-url("../../../assets/icons/nav-activity.svg"); title: GuiI18n.text("gui-no-recent-activity"); message: GuiI18n.text("gui-no-recent-activity-detail"); }
                            ScrollView { viewport-width: self.width; VerticalLayout { spacing: 10px; for item in root.activities : ActivityCard { item: item; cancel(id) => { root.cancel-operation(id); } } } }
                        }
                    }

                    // Settings.
                    if root.page == 4 : HorizontalLayout { spacing: 0px; horizontal-stretch: 1;
                        Rectangle { width: 208px; background: Theme.surface; border-width: 1px; border-color: Theme.border; VerticalLayout { padding: 14px; spacing: 5px; Eyebrow { text: GuiI18n.text("gui-settings-heading"); }
                            Rectangle { height: 5px; }
                            SettingsNavItem { label: GuiI18n.text("gui-settings-general"); selected: root.workspace-tab == "General"; clicked => { root.workspace-tab = "General"; } }
                            SettingsNavItem { label: GuiI18n.text("gui-settings-minecraft"); selected: root.workspace-tab == "Minecraft"; clicked => { root.workspace-tab = "Minecraft"; root.request-version-catalog(); } }
                            SettingsNavItem { label: GuiI18n.text("gui-settings-java"); selected: root.workspace-tab == "Java"; clicked => { root.workspace-tab = "Java"; } }
                            SettingsNavItem { label: GuiI18n.text("gui-settings-accounts"); selected: root.workspace-tab == "Accounts"; clicked => { root.workspace-tab = "Accounts"; } }
                            SettingsNavItem { label: GuiI18n.text("gui-settings-providers"); selected: root.workspace-tab == "Providers & APIs"; clicked => { root.workspace-tab = "Providers & APIs"; } }
                            // Measuring walks the cache, so it happens when the page
                            // that shows the figure is opened rather than on every
                            // refresh of everything else.
                            SettingsNavItem { label: GuiI18n.text("gui-settings-storage"); selected: root.workspace-tab == "Storage"; clicked => { root.workspace-tab = "Storage"; root.measure-artifact-cache(); } }
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
                                    HorizontalLayout { padding: 16px; spacing: 20px; cross-axis-alignment: center;
                                        VerticalLayout { horizontal-stretch: 1; spacing: 5px; Heading { text: GuiI18n.text("gui-application-text-size"); font-size: Theme.row-heading-size; } BodyText { text: GuiI18n.text("gui-application-text-size-detail"); wrap: word-wrap; } }
                                        ComboBox { width: 190px; model: [GuiI18n.text("gui-text-scale-90"), GuiI18n.text("gui-text-scale-100"), GuiI18n.text("gui-text-scale-110"), GuiI18n.text("gui-text-scale-125")]; current-index: root.application-text-scale == 90 ? 0 : root.application-text-scale == 110 ? 2 : root.application-text-scale == 125 ? 3 : 1; selected(value) => { root.application-text-scale = value == GuiI18n.text("gui-text-scale-90") ? 90 : value == GuiI18n.text("gui-text-scale-110") ? 110 : value == GuiI18n.text("gui-text-scale-125") ? 125 : 100; } }
                                    }
                                }
                                Rectangle { height: root.application-text-scale >= 125 ? 124px : root.application-text-scale >= 110 ? 112px : 104px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    HorizontalLayout { padding: 16px; spacing: 20px; cross-axis-alignment: center;
                                        VerticalLayout { horizontal-stretch: 1; spacing: 5px; Heading { text: GuiI18n.text("gui-technical-text-size"); font-size: Theme.row-heading-size; } BodyText { text: GuiI18n.text("gui-technical-text-size-detail"); wrap: word-wrap; } }
                                        ComboBox { width: 190px; model: [GuiI18n.text("gui-technical-size-11"), GuiI18n.text("gui-technical-size-12"), GuiI18n.text("gui-technical-size-14"), GuiI18n.text("gui-technical-size-16")]; current-index: root.technical-text-size == 11 ? 0 : root.technical-text-size == 14 ? 2 : root.technical-text-size == 16 ? 3 : 1; selected(value) => { root.technical-text-size = value == GuiI18n.text("gui-technical-size-11") ? 11 : value == GuiI18n.text("gui-technical-size-14") ? 14 : value == GuiI18n.text("gui-technical-size-16") ? 16 : 12; } }
                                    }
                                }
                                ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-display-settings"); tone: "primary"; clicked => { root.save-display-settings(root.application-text-scale, root.technical-text-size); } } }
                                SectionTitle { title: GuiI18n.text("gui-linked-server-defaults"); detail: GuiI18n.text("gui-linked-server-defaults-detail"); }
                                LabeledToggle { width: min(parent.width, 520px); label: GuiI18n.text("gui-show-linked-servers"); checked <=> root.show-linked-servers; }
                                LabeledToggle { width: min(parent.width, 520px); label: GuiI18n.text("gui-default-account-operator"); checked <=> root.make-default-account-operator; }
                                HorizontalLayout { spacing: 12px; cross-axis-alignment: center; VerticalLayout { FieldLabel { text: GuiI18n.text("gui-default-operator-level"); } BodyText { text: GuiI18n.text("gui-default-operator-level-detail"); color: Theme.text-muted; } } Rectangle { horizontal-stretch: 1; } ComboBox { accessible-label: GuiI18n.text("gui-default-operator-level"); width: 220px; model: [GuiI18n.text("gui-operator-level-1"), GuiI18n.text("gui-operator-level-2"), GuiI18n.text("gui-operator-level-3"), GuiI18n.text("gui-operator-level-4")]; current-index: root.operator-permission-level - 1; selected(value) => { root.operator-permission-level = value == GuiI18n.text("gui-operator-level-1") ? 1 : value == GuiI18n.text("gui-operator-level-2") ? 2 : value == GuiI18n.text("gui-operator-level-3") ? 3 : 4; } } }
                                ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-server-defaults"); tone: "primary"; clicked => { root.save-linked-server-settings(root.show-linked-servers, root.make-default-account-operator, root.operator-permission-level); } } }
                                SectionTitle { title: GuiI18n.text("gui-favorite-versions"); detail: GuiI18n.text("gui-favorite-versions-detail"); }
                                if root.favorite-minecraft-versions.length == 0 : Rectangle { height: max(46px, no-favorites.preferred-height + 22px); border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    no-favorites := BodyText { x: 12px; width: parent.width - 24px; text: GuiI18n.text("gui-no-favorite-versions"); color: Theme.text-muted; wrap: word-wrap; }
                                }
                                for version in root.favorite-minecraft-versions : Rectangle {
                                    width: min(parent.width, 520px);
                                    height: 48px;
                                    border-radius: 6px;
                                    border-width: 1px;
                                    border-color: Theme.border;
                                    background: Theme.surface;
                                    HorizontalLayout {
                                        padding-left: 14px; padding-right: 8px; padding-top: 6px; padding-bottom: 6px; spacing: 10px;
                                        BodyText { text: version; color: Theme.text; horizontal-stretch: 1; vertical-alignment: center; overflow: elide; }
                                        VerticalLayout { alignment: center; FavoriteToggle { version: version; favorite: true; toggled => { root.remove-favorite-version(version); } } }
                                    }
                                }
                            }
                            // The screen keeps its own height and scrolls only the
                            // rows, so the category tabs, the filter, and Save and
                            // Discard stay reachable however long the list is.
                            if root.workspace-tab == "Minecraft" : VerticalLayout { spacing: 12px;
                                height: max(360px, root.height - 258px);
                                if root.minecraft-preserved-notice != "" : Rectangle { height: max(46px, preserved-text.preferred-height + 22px); border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.surface;
                                    preserved-text := BodyText { x: 12px; width: parent.width - 24px; text: root.minecraft-preserved-notice; color: Theme.text-muted; wrap: word-wrap; }
                                }
                                if root.minecraft-unusable-notice != "" : Rectangle { height: max(46px, unusable-text.preferred-height + 22px); border-radius: 6px; border-width: 1px; border-color: Theme.warning.with-alpha(0.55); background: Theme.surface;
                                    unusable-text := BodyText { x: 12px; width: parent.width - 24px; text: root.minecraft-unusable-notice; color: Theme.warning; wrap: word-wrap; }
                                }
                                // The five stable catalog categories. They wrap
                                // rather than shrink, so a narrow window keeps
                                // every tab readable and reachable.
                                Rectangle { height: category-tabs.preferred-height; vertical-stretch: 0; background: transparent;
                                    category-tabs := HorizontalLayout { spacing: 4px; alignment: start;
                                        for category[index] in root.minecraft-categories : WorkspaceTab {
                                            text: category;
                                            min-width: max(88px, self.preferred-width);
                                            selected: index == root.minecraft-category-index;
                                            clicked => { root.minecraft-select-category(index); }
                                        }
                                    }
                                }
                                Divider { }
                                HorizontalLayout { spacing: 10px; cross-axis-alignment: center; vertical-stretch: 0;
                                    filter-field := TextField {
                                        horizontal-stretch: 1;
                                        max-width: 340px;
                                        placeholder: GuiI18n.text("gui-minecraft-filter");
                                        accessible-name: GuiI18n.text("gui-minecraft-filter");
                                        input-type: InputType.search;
                                        edited(value) => { root.minecraft-query = value; root.minecraft-set-query(value); }
                                    }
                                    if root.minecraft-query != "" : AppButton { text: GuiI18n.text("gui-clear"); min-width: 84px; clicked => { filter-field.text = ""; root.minecraft-query = ""; root.minecraft-set-query(""); } }
                                    Rectangle { horizontal-stretch: 1; min-width: 4px; }
                                    BodyText { text: GuiI18n.text("gui-minecraft-only-overridden"); horizontal-stretch: 0; }
                                    Toggle { accessible-name: GuiI18n.text("gui-minecraft-only-overridden"); checked: root.minecraft-only-overridden; toggled(value) => { root.minecraft-set-only-overridden(value); } }
                                    BodyText { text: GuiI18n.text("gui-minecraft-show-advanced"); horizontal-stretch: 0; }
                                    Toggle { accessible-name: GuiI18n.text("gui-minecraft-show-advanced"); checked: root.minecraft-show-advanced; toggled(value) => { root.minecraft-set-show-advanced(value); } }
                                }
                                HorizontalLayout { spacing: 10px; cross-axis-alignment: center; vertical-stretch: 0;
                                    VerticalLayout { horizontal-stretch: 1; spacing: 2px;
                                        FieldLabel { text: GuiI18n.text("gui-minecraft-preview"); }
                                        BodyText { text: GuiI18n.text("gui-minecraft-preview-detail"); color: Theme.text-muted; wrap: word-wrap; }
                                    }
                                    preview-box := ComboBox {
                                        width: root.compact-layout ? 150px : 190px;
                                        accessible-label: GuiI18n.text("gui-minecraft-preview");
                                        model: root.minecraft-releases;
                                        current-value: root.minecraft-preview-release;
                                        selected(value) => { root.minecraft-set-preview-release(value); }
                                    }
                                }
                                ScrollView { vertical-stretch: 1; viewport-width: self.width; VerticalLayout { alignment: start; spacing: 12px;
                                if root.minecraft-preference-rows.length == 0 : EmptyState { title: GuiI18n.text("gui-minecraft-no-matches"); message: GuiI18n.text("gui-minecraft-no-matches-detail"); }
                                for row in root.minecraft-preference-rows : MinecraftPreferenceRowCard {
                                    item: row;
                                    compact: root.compact-layout;
                                    capturing: root.minecraft-capturing-id == row.id;
                                    languages-available: root.minecraft-languages-available;
                                    set-boolean(id, on) => { root.minecraft-set-boolean(id, on); }
                                    set-number(id, value) => { root.minecraft-set-number(id, value); }
                                    edit-number(id, value) => { root.minecraft-edit-number(id, value); }
                                    commit-number(id, value) => { root.minecraft-commit-number(id, value); }
                                    set-enumeration(id, index) => { root.minecraft-set-enumeration(id, index); }
                                    set-language(id, index) => { root.minecraft-set-language(id, index); }
                                    aspect-labels: root.minecraft-aspect-labels;
                                    edit-window(id, width, height) => { root.minecraft-edit-window(id, width, height); }
                                    commit-window-width(id, width, height) => { root.minecraft-commit-window-width(id, width, height); }
                                    commit-window-height(id, width, height) => { root.minecraft-commit-window-height(id, width, height); }
                                    set-window-aspect(id, index) => { root.minecraft-set-window-aspect(id, index); }
                                    set-window-lock(id, held) => { root.minecraft-set-window-lock(id, held); }
                                    open-window-preview(id) => { root.minecraft-open-window-preview(id); }
                                    commit-text(id, value) => { root.minecraft-commit-text(id, value); }
                                    forget(id) => { root.minecraft-forget-preference(id); }
                                    begin-capture(id) => { root.minecraft-begin-capture(id); }
                                    cancel-capture => { root.minecraft-cancel-capture(); }
                                    captured-key(id, reported) => { root.minecraft-captured-key(id, reported); }
                                    captured-mouse(id, reported) => { root.minecraft-captured-mouse(id, reported); }
                                    retry-languages => { root.minecraft-retry-languages(); }
                                }
                                } }
                                Divider { }
                                HorizontalLayout { spacing: 10px; cross-axis-alignment: center; vertical-stretch: 0;
                                    BodyText { text: root.minecraft-dirty ? GuiI18n.count("gui-minecraft-unsaved-count", root.minecraft-dirty-count) : GuiI18n.count("gui-minecraft-total-count", root.minecraft-total-count); color: root.minecraft-dirty ? Theme.accent : Theme.text-muted; horizontal-stretch: 1; wrap: word-wrap; }
                                    AppButton { text: GuiI18n.text("gui-discard-changes"); enabled: root.minecraft-dirty && !root.minecraft-saving; clicked => { root.minecraft-discard-defaults(); } }
                                    AppButton { text: GuiI18n.text("gui-save-changes"); tone: "primary"; enabled: root.minecraft-can-save && !root.minecraft-saving; clicked => { root.minecraft-save-defaults(); } }
                                }
                            }
                            if root.workspace-tab == "Java" : VerticalLayout { spacing: 12px;
                                VerticalLayout { spacing: 6px;
                                    LabeledToggle { width: min(parent.width, 440px); label: GuiI18n.text("gui-java-detect"); checked <=> root.java-auto-detect; }
                                    LabeledToggle { width: min(parent.width, 440px); label: GuiI18n.text("gui-java-download"); checked <=> root.java-auto-download; }
                                }
                                HorizontalLayout { spacing: 12px; VerticalLayout { FieldLabel { text: GuiI18n.text("gui-java-memory-min"); } TextField { text <=> root.java-memory-min; input-type: InputType.number; width: 190px; } } VerticalLayout { FieldLabel { text: GuiI18n.text("gui-java-memory-max"); } TextField { text <=> root.java-memory-max; input-type: InputType.number; width: 190px; } } }
                                Text { text: GuiI18n.text("gui-java-preferred"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.java-preferred-path; placeholder: "/usr/bin/java"; }
                                ActionRow { AppButton { text: GuiI18n.text("gui-java-refresh"); clicked => { root.refresh-java(); } } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-java-save"); tone: "primary"; clicked => { root.save-java-settings(root.java-auto-detect, root.java-auto-download, root.java-memory-min, root.java-memory-max, root.java-preferred-path); } } }
                                for java in root.java-runtimes : Rectangle { height: 75px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; HorizontalLayout { padding: 12px; VerticalLayout { Heading { text: java.version + " | " + java.vendor; font-size: 14px; } BodyText { text: java.path; overflow: elide; } } Pill { text: java.origin; tone-color: Theme.accent-secondary; } } }
                            }
                            if root.workspace-tab == "Accounts" : VerticalLayout { spacing: 12px;
                                TextField { text <=> root.offline-account-name; placeholder: GuiI18n.text("gui-offline-account-placeholder"); }
                                HorizontalLayout { spacing: 8px; AppButton { text: GuiI18n.text("gui-add-offline-account"); width: 180px; clicked => { root.add-offline-account(root.offline-account-name); } } AppButton { text: GuiI18n.text("gui-microsoft-sign-in"); width: 205px; tone: "primary"; clicked => { root.begin-microsoft-login(); } } Rectangle { horizontal-stretch: 1; } }
                                if root.accounts.length == 0 : EmptyState { title: GuiI18n.text("gui-no-accounts"); message: GuiI18n.text("gui-no-accounts-detail"); }
                                for account in root.accounts : Rectangle { height: 78px; border-radius: 8px; border-width: account.is-default ? 2px : 1px; border-color: account.is-default ? Theme.accent : Theme.border; background: Theme.surface; accessible-role: list-item; accessible-label: account.name; HorizontalLayout { padding: 12px; spacing: 8px; cross-axis-alignment: center; VerticalLayout { alignment: center; horizontal-stretch: 1; Heading { text: account.name; font-size: 14px; } BodyText { text: account.kind + " | " + account.credential-state; } }
                                    // The marker stands where the button it replaces would be, at
                                    // that button's size, so the column does not jump between one
                                    // row and the next.
                                    if account.is-default : Pill { text: GuiI18n.text("gui-default"); tone-color: Theme.accent; min-content-width: 130px; content-height: Theme.control-height; }
                                    if !account.is-default : AppButton { text: GuiI18n.text("gui-make-default"); width: 130px; clicked => { root.set-default-account(account.id); } }
                                    AppButton { text: GuiI18n.text("gui-remove"); width: 110px; tone: "danger"; clicked => { root.remove-account(account.id); } } } }
                            }
                            if root.workspace-tab == "Providers & APIs" : VerticalLayout { spacing: 14px;
                                SectionTitle { title: GuiI18n.text("gui-preferred-mod-catalog"); detail: GuiI18n.text("gui-preferred-mod-catalog-detail"); }
                                Rectangle { height: 146px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 16px; spacing: 8px;
                                    HorizontalLayout { Heading { text: GuiI18n.text("gui-preferred-mod-catalog-heading"); font-size: Theme.row-heading-size; } Rectangle { horizontal-stretch: 1; } Pill { text: root.default-mod-catalog-is-custom ? GuiI18n.text("gui-override") : GuiI18n.text("gui-default"); tone-color: root.default-mod-catalog-is-custom ? Theme.accent : Theme.text-secondary; } }
                                    // Applied the moment a chip is clicked, and the selection follows
                                    // the stored value rather than the click: a save that fails leaves
                                    // the previous catalog selected instead of the one that was asked for.
                                    HorizontalLayout { spacing: 8px; FilterChip { text: GuiI18n.text("gui-provider-modrinth"); accessible-name: GuiI18n.text("gui-provider-modrinth"); selected: root.default-mod-catalog == "modrinth"; clicked => { root.choose-default-mod-catalog("modrinth"); } } FilterChip { text: GuiI18n.text("gui-provider-curseforge"); accessible-name: GuiI18n.text("gui-provider-curseforge"); selected: root.default-mod-catalog == "curseforge"; clicked => { root.choose-default-mod-catalog("curseforge"); } } Rectangle { horizontal-stretch: 1; } if root.default-mod-catalog-is-custom : AppButton { text: GuiI18n.text("gui-reset-default"); clicked => { root.choose-default-mod-catalog(""); } } }
                                    BodyText { text: root.default-mod-catalog == "" ? GuiI18n.value("gui-preferred-mod-catalog-unrecognized", root.configured-mod-catalog) : GuiI18n.text("gui-preferred-mod-catalog-note"); color: Theme.text-muted; wrap: word-wrap; }
                                } }
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
                                ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-api-settings"); tone: "primary"; clicked => { root.save-api-settings(root.microsoft-client-id, root.user-agent); } } }
                                SectionTitle { title: GuiI18n.text("gui-curseforge"); detail: GuiI18n.text("gui-curseforge-detail"); }
                                Rectangle { height: 196px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 16px; spacing: 8px;
                                    HorizontalLayout { Heading { text: GuiI18n.text("gui-curseforge-api-key"); font-size: Theme.row-heading-size; } Rectangle { horizontal-stretch: 1; } Pill { text: root.curseforge-key-configured ? GuiI18n.text("gui-curseforge-key-configured") : GuiI18n.text("gui-curseforge-key-not-configured"); tone-color: root.curseforge-key-configured ? Theme.accent : Theme.text-secondary; } }
                                    // Masked because it shows what the user is typing, never what is stored:
                                    // a saved key is not readable from here and the field is emptied on submit.
                                    HorizontalLayout { spacing: 8px; TextField { text <=> root.curseforge-api-key; placeholder: GuiI18n.text("gui-curseforge-key-placeholder"); accessible-name: GuiI18n.text("gui-curseforge-api-key"); input-type: InputType.password; horizontal-stretch: 1; accepted => { root.save-curseforge-api-key(root.curseforge-api-key); } } AppButton { text: root.curseforge-key-configured ? GuiI18n.text("gui-curseforge-key-replace") : GuiI18n.text("gui-curseforge-key-save"); tone: "primary"; enabled: root.curseforge-api-key != ""; clicked => { root.save-curseforge-api-key(root.curseforge-api-key); } } if root.curseforge-key-configured : AppButton { text: GuiI18n.text("gui-remove"); tone: "danger"; clicked => { root.remove-curseforge-api-key(); } } }
                                    BodyText { text: GuiI18n.text("gui-curseforge-key-storage"); color: Theme.text-muted; wrap: word-wrap; }
                                    HorizontalLayout { AppButton { text: GuiI18n.text("gui-curseforge-key-help"); clicked => { root.open-curseforge-key-help(); } } Rectangle { horizontal-stretch: 1; } }
                                } }
                            }
                            if root.workspace-tab == "Storage" : VerticalLayout { spacing: 14px;
                                SectionTitle { title: GuiI18n.text("gui-persistent-storage"); detail: GuiI18n.text("gui-persistent-storage-detail"); }
                                Rectangle { height: 164px; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; VerticalLayout { padding: 16px; spacing: 9px;
                                    HorizontalLayout { Heading { text: GuiI18n.text("gui-storage-root"); font-size: Theme.row-heading-size; } Rectangle { horizontal-stretch: 1; } Pill { text: root.persistent-root-is-custom ? GuiI18n.text("gui-custom-location") : GuiI18n.text("gui-default-location"); tone-color: root.persistent-root-is-custom ? Theme.accent : Theme.text-secondary; } }
                                    HorizontalLayout { spacing: 8px; TextField { text <=> root.persistent-root; placeholder: GuiI18n.text("gui-no-override"); horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-choose-folder"); clicked => { root.choose-storage-root(); } } }
                                    BodyText { text: GuiI18n.value("gui-effective-location", root.storage-root); color: Theme.text-secondary; overflow: elide; }
                                    BodyText { text: GuiI18n.value("gui-default-location-path", root.default-persistent-root); color: Theme.text-muted; overflow: elide; }
                                } }
                                ActionRow { if root.persistent-root-is-custom : AppButton { text: GuiI18n.text("gui-reset-default"); clicked => { root.reset-storage-root(); } } Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-save-storage-root"); tone: "primary"; clicked => { root.save-storage-settings(root.persistent-root); } } }
                                BodyText { text: GuiI18n.text("gui-storage-change-future-only"); color: Theme.text-muted; wrap: word-wrap; }
                                Divider { }
                                SectionTitle { title: GuiI18n.text("gui-watched-download-folders"); detail: GuiI18n.text("gui-watched-download-folders-detail"); }
                                if root.watched-download-folders.length > 0 : VerticalLayout {
                                    spacing: 6px;
                                    // One width for every button in this
                                    // column, and the standard control height:
                                    // a row that shrinks its own button leaves
                                    // the one below it a different size.
                                    for folder in root.watched-download-folders : ActionRow {
                                        BodyText { text: folder; overflow: elide; vertical-alignment: center; horizontal-stretch: 1; }
                                        AppButton { width: 128px; text: GuiI18n.text("gui-remove"); tone: "danger"; clicked => { root.remove-watched-folder(folder); } }
                                    }
                                }
                                if root.watched-download-folders.length == 0 : BodyText { text: GuiI18n.text("gui-watched-download-folders-empty"); color: Theme.text-muted; wrap: word-wrap; }
                                ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { width: 128px; text: GuiI18n.text("gui-add-watched-folder"); clicked => { root.choose-watched-folder(); } } }
                                Divider { }
                                SectionTitle { title: GuiI18n.text("gui-artifact-cache"); detail: GuiI18n.text("gui-artifact-cache-detail"); }
                                // Height taken from the content rather than fixed:
                                // a bare Rectangle reports no preferred height of
                                // its own, and the scroll view needs a real one or
                                // the row below this card cannot be reached.
                                Rectangle { height: cache-card.preferred-height; border-radius: 8px; border-width: 1px; border-color: Theme.border; background: Theme.surface; cache-card := VerticalLayout { padding: 16px; spacing: 12px;
                                    HorizontalLayout { spacing: 10px;
                                        Heading { text: root.artifact-cache-usage-known ? (root.artifact-cache-usage-exact ? GuiI18n.values("gui-artifact-cache-usage", root.artifact-cache-used, root.artifact-cache-total) : GuiI18n.values("gui-artifact-cache-usage-approximate", root.artifact-cache-used, root.artifact-cache-total)) : GuiI18n.text("gui-artifact-cache-measuring"); font-size: Theme.row-heading-size; overflow: elide; horizontal-stretch: 1; }
                                        if root.artifact-cache-over-budget : Pill { text: GuiI18n.text("gui-artifact-cache-over-budget"); tone-color: Theme.warning; }
                                    }
                                    HorizontalLayout { spacing: 10px; alignment: start;
                                        VerticalLayout { spacing: 4px;
                                            FieldLabel { text: GuiI18n.text("gui-artifact-cache-limit"); }
                                            ComboBox {
                                                width: 170px;
                                                accessible-label: GuiI18n.text("gui-artifact-cache-limit");
                                                model: root.artifact-cache-limit-labels;
                                                current-index <=> root.artifact-cache-limit-index;
                                            }
                                        }
                                        if root.artifact-cache-limit-index == root.artifact-cache-custom-index : VerticalLayout { spacing: 4px;
                                            FieldLabel { text: GuiI18n.text("gui-artifact-cache-custom-value"); }
                                            // Wide enough for a value far past any
                                            // preset: the presets stop at 100 GiB,
                                            // the setting does not.
                                            TextField { width: 130px; text <=> root.artifact-cache-custom-value; placeholder: "500"; input-type: InputType.number; accessible-name: GuiI18n.text("gui-artifact-cache-custom-value"); }
                                        }
                                        if root.artifact-cache-limit-index == root.artifact-cache-custom-index : VerticalLayout { spacing: 4px;
                                            FieldLabel { text: " "; }
                                            ComboBox { width: 100px; accessible-label: GuiI18n.text("gui-artifact-cache-custom-value"); model: root.artifact-cache-units; current-value <=> root.artifact-cache-custom-unit; }
                                        }
                                        VerticalLayout { spacing: 4px;
                                            // An empty label so the button lines up
                                            // with the controls it saves rather than
                                            // with their labels.
                                            FieldLabel { text: " "; }
                                            AppButton { text: GuiI18n.text("gui-save-artifact-cache-limit"); tone: "primary"; clicked => { root.save-artifact-cache-limit(root.artifact-cache-limit-index, root.artifact-cache-custom-value, root.artifact-cache-custom-unit); } }
                                        }
                                    }
                                    BodyText { text: GuiI18n.text("gui-artifact-cache-cleanup-note"); color: Theme.text-muted; wrap: word-wrap; }
                                } }
                                HorizontalLayout {
                                    // Named for the cache and nothing else: an
                                    // instance or a server is never what this
                                    // removes, and the button must not read as if it
                                    // might be.
                                    AppButton { text: root.artifact-cache-clearing ? GuiI18n.text("gui-clearing-artifact-cache") : GuiI18n.text("gui-clear-artifact-cache"); enabled: !root.artifact-cache-clearing; clicked => { root.artifact-cache-clearing = true; root.clear-artifact-cache(); } }
                                    Rectangle { horizontal-stretch: 1; }
                                }
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

        // What a file held over the window looks like. Drawn under the modal
        // overlay, and never while one is open, because a drop is refused then.
        if root.file-drop-active : Rectangle {
            background: Theme.background.with-alpha(0.8);
            Rectangle {
                width: min(520px, parent.width - 80px);
                height: 168px;
                border-radius: 10px;
                border-width: 2px;
                border-color: Theme.accent;
                background: Theme.elevated;
                VerticalLayout {
                    padding: 24px; spacing: 8px; alignment: center;
                    Heading { text: GuiI18n.text("gui-drop-to-import"); font-size: Theme.item-title-size; horizontal-alignment: center; }
                    BodyText { text: GuiI18n.text("gui-drop-to-import-detail"); wrap: word-wrap; horizontal-alignment: center; }
                }
            }
        }

        // Modal overlay and focused workflows.
        if root.modal != "" : Rectangle {
            background: #00000099;
            TouchArea { clicked => { } }
            Rectangle {
                width: root.modal == "license" ? min(820px, parent.width - 80px) : min(620px, parent.width - 80px);
                height: root.modal == "license" ? min(720px, parent.height - 80px) : root.modal == "third-party" ? 340px : root.modal == "import" ? min(620px, parent.height - 80px) : root.modal == "mod-plan" ? min(430px + root.linked-targets.length * 68px, parent.height - 80px) : root.modal == "mod-manage" ? min(620px, parent.height - 80px) : root.modal == "mod-apply" ? min(300px + root.linked-targets.length * 68px, parent.height - 80px) : root.modal == "mod-result" ? min(240px + root.linked-results.length * 68px, parent.height - 80px) : root.modal == "loader-change" ? 400px : root.modal == "mod-remove" ? min((root.linked-targets.length > 0 ? 340px : 250px) + root.mod-removal-dependencies.length * 24px + root.linked-targets.length * 68px, parent.height - 80px) : root.modal == "eula" ? 440px : root.modal == "kill" ? 300px : root.modal == "delete-instance" || root.modal == "delete-server" ? 330px : root.modal == "create-server" ? min((root.create-server-client-id == "" ? 640px : 535px) + (root.create-server-client-id == "" && root.create-server-loader != "Vanilla" ? 44px : 0px), parent.height - 80px) : 570px;
                x: (parent.width - self.width) / 2;
                y: (parent.height - self.height) / 2;
                border-radius: 8px;
                border-width: 1px;
                border-color: Theme.border;
                background: Theme.elevated;
                VerticalLayout { padding: 22px; spacing: 13px;
                    HorizontalLayout { cross-axis-alignment: center; Heading { text: root.modal == "create-instance" ? GuiI18n.text("gui-modal-new-instance") : root.modal == "import" ? GuiI18n.text("gui-modal-import") : root.modal == "create-server" ? GuiI18n.text("gui-modal-new-server") : root.modal == "mod-plan" ? GuiI18n.text("gui-modal-install-mod") : root.modal == "mod-manage" ? GuiI18n.text("gui-modal-manage-mod") : root.modal == "loader-change" ? GuiI18n.text("gui-modal-change-loader") : root.modal == "mod-remove" ? GuiI18n.text("gui-modal-remove-mod") : root.modal == "mod-apply" ? GuiI18n.text("gui-modal-apply-mod") : root.modal == "mod-result" ? GuiI18n.text("gui-modal-mod-result") : root.modal == "eula" ? GuiI18n.text("gui-server-eula") : root.modal == "kill" ? GuiI18n.text("gui-modal-force-kill") : root.modal == "delete-instance" ? GuiI18n.text("gui-delete-instance") : root.modal == "delete-server" ? GuiI18n.text("gui-delete-server") : root.modal == "license" ? GuiI18n.text("gui-modal-apache-license") : GuiI18n.text("gui-third-party-licenses"); font-size: 20px; } Rectangle { horizontal-stretch: 1; } IconButton { icon: @image-url("../../../assets/icons/close.svg"); accessible-name: GuiI18n.text("gui-close"); clicked => { if root.modal == "import" { root.close-import-modal(); } else { if root.modal == "mod-plan" { root.abandon-browser-install(); } root.modal = ""; root.modal-error = ""; } } } }
                    Divider { }
                    if root.modal == "create-instance" : VerticalLayout { spacing: 11px; BodyText { text: GuiI18n.text("gui-create-instance-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-name"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-name; placeholder: root.create-name-suggestion; accessible-name: GuiI18n.text("gui-name"); } Text { text: GuiI18n.text("gui-minecraft-release"); color: Theme.text-secondary; font-size: 12px; } HorizontalLayout { spacing: 8px; ComboBox { horizontal-stretch: 1; model: root.minecraft-releases; current-value <=> root.create-minecraft; selected(value) => { root.create-minecraft = value; root.create-loader-version = ""; root.create-loader-versions = []; root.minecraft-selection-changed("client", value); if root.create-loader != "Vanilla" { root.request-loader-catalog("client", root.create-loader, value); } root.recompute-create-name(); } } FavoriteToggle { version: root.create-minecraft; favorite: root.create-minecraft-favorite; enabled: root.create-minecraft != ""; toggled => { root.toggle-favorite-version(root.create-minecraft); } } } Text { text: GuiI18n.text("gui-loader"); color: Theme.text-secondary; font-size: 12px; } ComboBox { model: root.create-loaders; current-value <=> root.create-loader; selected(value) => { root.create-loader = value; root.create-loader-version = ""; root.create-loader-versions = []; if value != "Vanilla" { root.request-loader-catalog("client", value, root.create-minecraft); } root.recompute-create-name(); } } if root.create-loader != "Vanilla" : ComboBox { model: root.create-loader-versions; current-value <=> root.create-loader-version; changed current-value => { root.recompute-create-name(); } } HorizontalLayout { BodyText { text: GuiI18n.text("gui-store-persistently"); } Toggle { checked <=> root.create-persistent; } BodyText { text: root.create-persistent ? GuiI18n.text("gui-persistent-root") : GuiI18n.text("gui-temporary-root"); color: Theme.text-muted; } } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-create-instance"); tone: "primary"; enabled: root.minecraft-releases.length > 0 && (root.create-name != "" || root.create-name-suggestion != "") && root.create-minecraft != "" && (root.create-loader == "Vanilla" || root.create-loader-version != ""); clicked => { root.submit-create-instance(root.create-name != "" ? root.create-name : root.create-name-suggestion, root.create-minecraft, root.create-loader, root.create-loader-version, root.create-persistent); } } } }
                    if root.modal == "import" : ScrollView {
                        viewport-width: self.width;
                        VerticalLayout {
                            spacing: 11px;
                            BodyText { text: GuiI18n.text("gui-import-detail"); wrap: word-wrap; }
                            Text { text: GuiI18n.text("gui-modpack-archive"); color: Theme.text-secondary; font-size: 12px; }
                            HorizontalLayout {
                                TextField {
                                    // Display-only: it always mirrors the archive
                                    // the current analysis is for, and Browse is
                                    // the only way to start a new one.
                                    text: root.import-path;
                                    placeholder: GuiI18n.text("gui-modpack-placeholder");
                                    horizontal-stretch: 1;
                                    enabled: false;
                                }
                                AppButton {
                                    text: GuiI18n.text("gui-browse");
                                    accessible-name: GuiI18n.text("gui-choose-modpack-archive");
                                    enabled: root.import-status-kind != "submitting";
                                    clicked => { root.choose-import-file(); }
                                }
                            }
                            if root.import-status-kind == "analyzing" || root.import-status-kind == "resolving" : HorizontalLayout {
                                spacing: 7px;
                                Spinner { width: 16px; height: 16px; }
                                BodyText {
                                    text: root.import-status-kind == "resolving" ? GuiI18n.text("gui-import-resolving") : GuiI18n.text("gui-import-analyzing");
                                    color: Theme.text-secondary;
                                }
                            }
                            if root.import-pack-header != "" : Rectangle {
                                border-radius: 6px;
                                border-width: 1px;
                                border-color: root.import-status-kind == "ready" || root.import-status-kind == "submitting" ? Theme.success : Theme.border;
                                background: Theme.surface;
                                VerticalLayout {
                                    padding: 12px;
                                    spacing: 4px;
                                    Heading { text: root.import-pack-header; font-size: 14px; wrap: word-wrap; }
                                    if root.import-author-line != "" : BodyText { text: root.import-author-line; color: Theme.text-secondary; wrap: word-wrap; }
                                    BodyText { text: root.import-minecraft-line; color: Theme.text-secondary; wrap: word-wrap; }
                                    BodyText { text: root.import-loader-line; color: Theme.text-secondary; wrap: word-wrap; }
                                    BodyText { text: root.import-required-line + " | " + root.import-optional-line; color: Theme.text-secondary; wrap: word-wrap; }
                                }
                            }
                            if root.import-optional-items.length > 0 : ScrollView {
                                viewport-width: self.width;
                                height: min(root.import-optional-items.length * 28px, 200px);
                                VerticalLayout {
                                    for item in root.import-optional-items : HorizontalLayout {
                                        spacing: 8px;
                                        height: 28px;
                                        Toggle {
                                            checked: item.selected;
                                            accessible-name: item.display;
                                            toggled(selected) => { root.toggle-import-optional(item.logical-path); }
                                        }
                                        BodyText { text: item.display; overflow: elide; vertical-alignment: center; }
                                    }
                                }
                            }
                            if root.import-blocked-items.length > 0 : VerticalLayout {
                                spacing: 6px;
                                BodyText { text: GuiI18n.text("gui-import-blocked-detail"); color: Theme.warning; wrap: word-wrap; }
                                HorizontalLayout {
                                    Rectangle { horizontal-stretch: 1; }
                                    AppButton {
                                        text: GuiI18n.text("gui-import-open-watched-folders");
                                        clicked => { root.close-import-modal(); root.page = 4; root.workspace = ""; root.workspace-tab = "Storage"; }
                                    }
                                }
                                ScrollView {
                                    viewport-width: self.width;
                                    height: min(root.import-blocked-items.length * 64px, 220px);
                                    VerticalLayout {
                                        spacing: 6px;
                                        for item in root.import-blocked-items : Rectangle {
                                            height: 58px;
                                            border-radius: 6px;
                                            border-width: 1px;
                                            border-color: Theme.border;
                                            background: Theme.surface;
                                            HorizontalLayout {
                                                padding: 8px;
                                                spacing: 8px;
                                                VerticalLayout {
                                                    alignment: center;
                                                    BodyText { text: item.project-line; overflow: elide; }
                                                    BodyText { text: item.file-line; color: Theme.text-muted; overflow: elide; }
                                                }
                                                Rectangle { horizontal-stretch: 1; }
                                                AppButton {
                                                    text: GuiI18n.text("gui-open-in-browser");
                                                    enabled: item.browser-url != "";
                                                    clicked => { root.open-blocked-file(item.browser-url); }
                                                }
                                                AppButton {
                                                    text: GuiI18n.text("gui-skip");
                                                    tone: "danger";
                                                    clicked => { root.skip-blocked-file(item.identity); }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Text { text: GuiI18n.text("gui-instance-name-optional"); color: Theme.text-secondary; font-size: 12px; }
                            TextField {
                                text <=> root.import-name;
                                placeholder: GuiI18n.text("gui-detected-pack-name");
                                enabled: root.import-status-kind != "submitting";
                                edited(text) => { root.import-name-edited(text); }
                            }
                            HorizontalLayout { BodyText { text: GuiI18n.text("gui-store-persistently"); } Toggle { checked <=> root.create-persistent; } }
                            HorizontalLayout { spacing: 10px; cross-axis-alignment: center; VerticalLayout { horizontal-stretch: 1; spacing: 2px; BodyText { text: GuiI18n.text("gui-import-create-server"); } BodyText { text: GuiI18n.text("gui-import-create-server-detail"); color: Theme.text-muted; wrap: word-wrap; } } Toggle { checked <=> root.import-create-server; accessible-name: GuiI18n.text("gui-import-create-server"); } }
                                if root.import-create-server : VerticalLayout { spacing: 6px;
                                    Text { text: GuiI18n.text("gui-server-pack"); color: Theme.text-secondary; font-size: 12px; }
                                    BodyText { text: GuiI18n.text("gui-server-pack-detail"); color: Theme.text-muted; wrap: word-wrap; }
                                    HorizontalLayout { spacing: 8px; alignment: start;
                                        AppButton { text: GuiI18n.text("gui-browse"); accessible-name: GuiI18n.text("gui-choose-server-pack"); clicked => { root.choose-import-server-pack(); } }
                                        if root.import-server-pack != "" : AppButton { text: GuiI18n.text("gui-clear"); accessible-name: GuiI18n.text("gui-clear-server-pack"); clicked => { root.clear-import-server-pack(); } }
                                        if root.import-server-pack != "" : VerticalLayout { alignment: center; horizontal-stretch: 1; BodyText { text: root.import-server-pack-name; overflow: elide; } }
                                    }
                                }
                            if root.modal-error != "" : VerticalLayout {
                                spacing: 6px;
                                BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; }
                                if root.import-error-kind == "missing-credential" || root.import-error-kind == "rejected-credential" : AppButton {
                                    text: GuiI18n.text("gui-import-open-providers");
                                    clicked => { root.close-import-modal(); root.page = 4; root.workspace = ""; root.workspace-tab = "Providers & APIs"; }
                                }
                            }
                            ActionRow {
                                Rectangle { horizontal-stretch: 1; }
                                AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.close-import-modal(); } }
                                AppButton {
                                    text: GuiI18n.text("gui-import-modpack");
                                    tone: "primary";
                                    enabled: root.import-ready;
                                    clicked => { root.submit-import(root.import-name, root.create-persistent, root.import-create-server, root.import-server-pack); }
                                }
                            }
                            // Absorbs whatever space the content above did not
                            // need, so sparse content (an idle picker, a short
                            // error) keeps its rows tight instead of spreading
                            // them across the scroll viewport.
                            Rectangle { vertical-stretch: 1; }
                        }
                    }
                    if root.modal == "create-server" : ScrollView { VerticalLayout { spacing: 10px; alignment: start; BodyText { text: root.create-server-client-id != "" ? GuiI18n.text("gui-create-linked-server-detail") : GuiI18n.text("gui-create-standalone-server-detail"); wrap: word-wrap; } Text { text: GuiI18n.text("gui-name"); color: Theme.text-secondary; font-size: 12px; } TextField { text <=> root.create-server-name; placeholder: root.create-server-name-suggestion; accessible-name: GuiI18n.text("gui-name"); } if root.create-server-client-id == "" : Text { text: GuiI18n.text("gui-minecraft-release"); color: Theme.text-secondary; font-size: 12px; } if root.create-server-client-id == "" : ComboBox { model: root.minecraft-releases; current-value <=> root.create-server-minecraft; selected(value) => { root.create-server-minecraft = value; root.create-server-loader-version = ""; root.create-server-loader-versions = []; root.minecraft-selection-changed("server", value); if root.create-server-loader != "Vanilla" { root.request-loader-catalog("server", root.create-server-loader, value); } root.recompute-create-server-name(); } } if root.create-server-client-id == "" : Text { text: GuiI18n.text("gui-loader"); color: Theme.text-secondary; font-size: 12px; } if root.create-server-client-id == "" : ComboBox { model: root.create-server-loaders; current-value <=> root.create-server-loader; selected(value) => { root.create-server-loader = value; root.create-server-loader-version = ""; root.create-server-loader-versions = []; if value != "Vanilla" { root.request-loader-catalog("server", value, root.create-server-minecraft); } root.recompute-create-server-name(); } } if root.create-server-client-id == "" && root.create-server-loader != "Vanilla" : ComboBox { model: root.create-server-loader-versions; current-value <=> root.create-server-loader-version; changed current-value => { root.recompute-create-server-name(); } } HorizontalLayout { BodyText { text: GuiI18n.text("gui-store-persistently"); } Toggle { checked <=> root.create-server-persistent; } } HorizontalLayout { BodyText { text: GuiI18n.text("gui-port-automatic"); } Toggle { checked <=> root.create-server-automatic-port; accessible-name: GuiI18n.text("gui-port-automatic"); } } if root.create-server-automatic-port : BodyText { text: GuiI18n.text("gui-port-automatic-detail"); color: Theme.text-muted; wrap: word-wrap; } if !root.create-server-automatic-port : HorizontalLayout { FieldLabel { text: GuiI18n.text("gui-port-custom"); } TextField { accessible-label: GuiI18n.text("gui-port-custom"); text <=> root.create-server-port; input-type: InputType.number; width: 150px; } } Text { text: GuiI18n.text("gui-server-pack"); color: Theme.text-secondary; font-size: 12px; } BodyText { text: GuiI18n.text("gui-server-pack-detail"); color: Theme.text-muted; wrap: word-wrap; } HorizontalLayout { spacing: 8px; alignment: start; AppButton { text: GuiI18n.text("gui-browse"); accessible-name: GuiI18n.text("gui-choose-server-pack"); clicked => { root.choose-server-pack(); } } if root.create-server-pack != "" : AppButton { text: GuiI18n.text("gui-clear"); accessible-name: GuiI18n.text("gui-clear-server-pack"); clicked => { root.clear-server-pack(); } } if root.create-server-pack != "" : VerticalLayout { alignment: center; horizontal-stretch: 1; BodyText { text: root.create-server-pack-name; overflow: elide; } } } BodyText { text: GuiI18n.text("gui-eula-create-warning"); color: Theme.warning; wrap: word-wrap; } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-create-server"); tone: "primary"; enabled: (root.create-server-name != "" || root.create-server-name-suggestion != "") && (root.create-server-client-id != "" || (root.minecraft-releases.length > 0 && root.create-server-minecraft != "" && (root.create-server-loader == "Vanilla" || root.create-server-loader-version != ""))); clicked => { root.submit-create-server(root.create-server-name != "" ? root.create-server-name : root.create-server-name-suggestion, root.create-server-client-id, root.create-server-minecraft, root.create-server-loader, root.create-server-loader-version, root.create-server-persistent, root.create-server-automatic-port, root.create-server-port, root.create-server-pack); } } } } }
                    if root.modal == "mod-plan" : VerticalLayout { spacing: 12px; Heading { text: root.mod-plan-title + " " + root.mod-plan-version; font-size: 16px; } BodyText { text: GuiI18n.text("gui-mod-plan-compatible"); wrap: word-wrap; } Rectangle { height: 116px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background; VerticalLayout { padding: 12px; spacing: 5px; BodyText { text: GuiI18n.count("gui-mod-plan-install", root.mod-plan-install-count); color: Theme.text; } BodyText { text: GuiI18n.count("gui-mod-plan-required", root.mod-plan-dependency-count); } BodyText { text: GuiI18n.count("gui-mod-plan-existing", root.mod-plan-existing-count); } BodyText { text: GuiI18n.count("gui-mod-plan-optional", root.mod-plan-optional-count); } } } if root.mod-plan-warning-count > 0 : BodyText { text: GuiI18n.count("gui-mod-plan-warnings", root.mod-plan-warning-count); color: Theme.warning; wrap: word-wrap; }
                        if root.linked-targets.length > 0 : Text { text: GuiI18n.text("gui-linked-servers"); color: Theme.text-secondary; font-size: Theme.label-size; }
                        if root.linked-targets.length > 0 && root.linked-support != "" : BodyText { text: root.linked-support; color: Theme.text-muted; wrap: word-wrap; }
                        if root.linked-targets.length > 0 : LinkedTargetsPanel { vertical-stretch: 1; rows: root.linked-targets; selectable: true; toggled(index) => { root.linked-targets[index].selected = !root.linked-targets[index].selected; } }
                        ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; root.abandon-browser-install(); } } AppButton { text: GuiI18n.text("gui-install-required"); tone: "primary"; enabled: root.mod-plan-install-count > 0 && !root.mod-action-pending; clicked => { root.mod-action-pending = true; root.install-mod(root.mod-plan-instance, root.mod-plan-provider, root.mod-plan-project, root.mod-plan-version-id); root.modal = ""; } } } }
                    if root.modal == "mod-manage" : VerticalLayout { spacing: 11px;
                        HorizontalLayout { spacing: 8px; Heading { text: root.mod-action-name; font-size: Theme.section-size; overflow: elide; horizontal-stretch: 1; } if root.mod-action-up-to-date : Pill { text: GuiI18n.text("gui-mod-up-to-date"); tone-color: Theme.success; } }
                        if root.mod-action-notice != "" : BodyText { text: root.mod-action-notice; color: root.mod-action-can-change ? Theme.text-muted : Theme.warning; wrap: word-wrap; }
                        if root.mod-action-loading : BodyText { text: GuiI18n.text("gui-mod-versions-loading"); color: Theme.text-muted; }
                        if !root.mod-action-loading && root.mod-versions.length == 0 && root.mod-action-notice == "" : BodyText { text: GuiI18n.text("gui-mod-no-compatible-versions"); color: Theme.text-muted; wrap: word-wrap; }
                        if root.mod-versions.length > 0 : Text { text: GuiI18n.text("gui-mod-compatible-versions"); color: Theme.text-secondary; font-size: Theme.label-size; }
                        if root.mod-versions.length > 0 : Rectangle {
                            vertical-stretch: 1; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background;
                            ScrollView { viewport-width: self.width; VerticalLayout { padding: 6px; spacing: 4px;
                                for entry in root.mod-versions : Rectangle {
                                    height: 46px; border-radius: 5px;
                                    border-width: entry.version-id == root.mod-action-selected ? 2px : 1px;
                                    border-color: entry.version-id == root.mod-action-selected ? Theme.accent : entry.installed ? Theme.accent-secondary.with-alpha(0.5) : Theme.border.with-alpha(0.6);
                                    background: entry.installed ? Theme.accent-secondary.with-alpha(0.08) : Theme.surface;
                                    HorizontalLayout { padding-left: 10px; padding-right: 10px; spacing: 8px;
                                        VerticalLayout { alignment: center; spacing: 2px; horizontal-stretch: 1;
                                            Text { text: entry.number; color: Theme.text; font-size: Theme.label-size; font-weight: 600; overflow: elide; }
                                            Text { text: entry.published + " | " + entry.channel; color: Theme.text-muted; font-size: Theme.caption-size; overflow: elide; }
                                        }
                                        VerticalLayout { alignment: center; if entry.installed : Pill { text: GuiI18n.text("gui-mod-installed-version"); tone-color: Theme.accent-secondary; } }
                                        VerticalLayout { alignment: center; if entry.newest && !entry.installed : Pill { text: GuiI18n.text("gui-mod-newest-version"); tone-color: Theme.success; } }
                                    }
                                    TouchArea { enabled: root.mod-action-can-change && !entry.installed; mouse-cursor: pointer; clicked => { root.mod-action-selected = entry.version-id; } }
                                }
                            } }
                        }
                        if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; }
                        HorizontalLayout { spacing: 8px;
                            if root.mod-action-can-remove : AppButton { text: GuiI18n.text("gui-remove"); tone: "danger"; enabled: !root.mod-action-pending; clicked => { root.modal-error = ""; root.plan-mod-removal(root.selected-instance-id, root.mod-action-path); } }
                            Rectangle { horizontal-stretch: 1; }
                            AppButton { text: GuiI18n.text("gui-close"); clicked => { root.modal = ""; root.modal-error = ""; } }
                            if root.mod-action-can-update : AppButton { text: GuiI18n.text("gui-mod-update"); enabled: !root.mod-action-pending; clicked => { root.mod-action-pending = true; root.modal-error = ""; root.plan-mod-apply(root.selected-instance-id, root.mod-action-path, ""); } }
                            if root.mod-action-can-change : AppButton { text: GuiI18n.text("gui-mod-change-version"); tone: "primary"; enabled: !root.mod-action-pending && root.mod-action-selected != "" && root.mod-action-selected != root.mod-action-installed; clicked => { root.mod-action-pending = true; root.modal-error = ""; root.plan-mod-apply(root.selected-instance-id, root.mod-action-path, root.mod-action-selected); } }
                        }
                    }
                    if root.modal == "mod-remove" : VerticalLayout { spacing: 12px;
                        BodyText { text: GuiI18n.value("gui-mod-remove-detail", root.mod-action-name); color: Theme.warning; wrap: word-wrap; }
                        if root.mod-removal-dependencies.length > 0 : BodyText { text: GuiI18n.count("gui-mod-remove-dependencies", root.mod-removal-dependencies.length); wrap: word-wrap; }
                        if root.mod-removal-dependencies.length > 0 : Rectangle {
                            // Sized to its own content so that the linked-server
                            // list beside it is not squeezed by an empty box.
                            vertical-stretch: 1; max-height: root.mod-removal-dependencies.length * 24px + 18px; border-radius: 6px; border-width: 1px; border-color: Theme.border; background: Theme.background;
                            ScrollView { viewport-width: self.width; VerticalLayout { alignment: start; padding: 9px; spacing: 4px; for name in root.mod-removal-dependencies : Text { text: name; color: Theme.text-secondary; font-size: Theme.label-size; height: 20px; overflow: elide; } } }
                        }
                        if root.mod-removal-dependencies.length == 0 && !root.mod-removal-unprovable : BodyText { text: GuiI18n.text("gui-mod-remove-no-dependencies"); color: Theme.text-muted; wrap: word-wrap; }
                        if root.mod-removal-unprovable : BodyText { text: GuiI18n.text("gui-mod-remove-dependencies-unprovable"); color: Theme.text-muted; wrap: word-wrap; }
                        if root.linked-targets.length > 0 : Text { text: GuiI18n.text("gui-linked-servers"); color: Theme.text-secondary; font-size: Theme.label-size; }
                        if root.linked-targets.length > 0 : LinkedTargetsPanel { vertical-stretch: 1; rows: root.linked-targets; selectable: true; toggled(index) => { root.linked-targets[index].selected = !root.linked-targets[index].selected; } }
                        if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; }
                        HorizontalLayout { spacing: 8px; Rectangle { horizontal-stretch: 1; }
                            AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = "mod-manage"; root.modal-error = ""; } }
                            AppButton { text: GuiI18n.text("gui-mod-remove-action"); tone: "danger"; enabled: !root.mod-action-pending; clicked => { root.modal-error = ""; root.remove-mod(root.selected-instance-id, root.mod-action-path); root.modal = ""; } }
                        }
                    }
                    if root.modal == "mod-apply" : VerticalLayout { spacing: 11px;
                        Heading { text: root.linked-name; font-size: Theme.section-size; overflow: elide; }
                        if root.linked-version-number != "" : BodyText { text: GuiI18n.value("gui-linked-exact-release", root.linked-version-number); wrap: word-wrap; }
                        if root.linked-support != "" : BodyText { text: root.linked-support; color: Theme.text-muted; wrap: word-wrap; }
                        Text { text: GuiI18n.text("gui-linked-servers"); color: Theme.text-secondary; font-size: Theme.label-size; }
                        LinkedTargetsPanel { vertical-stretch: 1; rows: root.linked-targets; selectable: true; toggled(index) => { root.linked-targets[index].selected = !root.linked-targets[index].selected; } }
                        if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; }
                        HorizontalLayout { spacing: 8px; Rectangle { horizontal-stretch: 1; }
                            AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = "mod-manage"; root.modal-error = ""; root.mod-action-pending = false; } }
                            AppButton { text: GuiI18n.text("gui-linked-apply"); tone: "primary"; enabled: !root.mod-action-pending; clicked => { root.modal-error = ""; root.change-mod-version(root.selected-instance-id, root.linked-path, root.linked-version); root.modal = ""; } }
                        }
                    }
                    if root.modal == "mod-result" : VerticalLayout { spacing: 11px;
                        Heading { text: root.linked-result-title; font-size: Theme.section-size; overflow: elide; }
                        BodyText { text: root.linked-result-summary; wrap: word-wrap; }
                        LinkedTargetsPanel { vertical-stretch: 1; rows: root.linked-results; }
                        ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-close"); tone: "primary"; clicked => { root.modal = ""; } } }
                    }
                    if root.modal == "loader-change" : VerticalLayout { spacing: 12px;
                        BodyText { text: GuiI18n.text("gui-loader-change-detail"); wrap: word-wrap; }
                        HorizontalLayout { InspectorLabel { text: GuiI18n.text("gui-loader-change-current"); } BodyText { text: root.loader-change-current != "" ? root.loader-change-current : GuiI18n.text("gui-mod-version-unknown"); } }
                        Text { text: GuiI18n.text("gui-loader-change-target"); color: Theme.text-secondary; font-size: Theme.label-size; }
                        if root.loader-change-loading : BodyText { text: GuiI18n.text("gui-mod-versions-loading"); color: Theme.text-muted; }
                        if !root.loader-change-loading && root.loader-change-versions.length > 0 : ComboBox { model: root.loader-change-versions; current-value <=> root.loader-change-selected; }
                        if !root.loader-change-loading && root.loader-change-versions.length == 0 : BodyText { text: GuiI18n.text("gui-loader-change-unavailable"); color: Theme.warning; wrap: word-wrap; }
                        if root.loader-change-unverifiable > 0 : BodyText { text: GuiI18n.count("gui-loader-change-unverifiable", root.loader-change-unverifiable); color: Theme.text-muted; wrap: word-wrap; }
                        BodyText { text: GuiI18n.text("gui-loader-change-mods-untouched"); color: Theme.text-muted; wrap: word-wrap; }
                        if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; }
                        HorizontalLayout { spacing: 8px; Rectangle { horizontal-stretch: 1; }
                            AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; root.modal-error = ""; } }
                            AppButton { text: GuiI18n.text("gui-loader-change-action"); tone: "primary"; enabled: !root.loader-change-pending && root.loader-change-selected != "" && root.loader-change-selected != root.loader-change-current; clicked => { root.loader-change-pending = true; root.modal-error = ""; root.change-loader-version(root.selected-instance-id, root.loader-change-selected); } }
                        }
                    }
                    if root.modal == "eula" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-eula-detail"); wrap: word-wrap; } AppButton { text: GuiI18n.text("gui-open-minecraft-eula"); clicked => { root.open-eula-reference(); } } Rectangle { height: 70px; border-radius: 6px; border-width: 1px; border-color: root.eula-accepted ? Theme.success : Theme.warning; background: Theme.surface; BodyText { x: 12px; width: parent.width - 24px; text: root.eula-status; wrap: word-wrap; } } if !root.eula-accepted : BodyText { text: GuiI18n.text("gui-eula-confirm"); color: Theme.text; wrap: word-wrap; } ActionRow { Rectangle { horizontal-stretch: 1; } if root.eula-accepted : AppButton { text: GuiI18n.text("gui-revoke-consent"); tone: "danger"; clicked => { root.revoke-eula(); } } if !root.eula-accepted : AppButton { text: GuiI18n.text("gui-accept-eula"); tone: "primary"; clicked => { root.accept-eula(); } } } }
                    if root.modal == "kill" : VerticalLayout { spacing: 14px; BodyText { text: GuiI18n.value("gui-force-kill-detail", root.pending-action-name != "" ? root.pending-action-name : root.selected-server-index >= 0 ? root.servers[root.selected-server-index].name : GuiI18n.text("gui-this-server")); color: Theme.warning; wrap: word-wrap; } ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-force-kill-action"); tone: "danger"; clicked => { root.server-kill(root.selected-server-id); root.modal = ""; } } } }
                    if root.modal == "delete-instance" || root.modal == "delete-server" : VerticalLayout { spacing: 14px; BodyText { text: GuiI18n.value(root.modal == "delete-instance" ? "gui-delete-instance-detail" : "gui-delete-server-detail", root.pending-action-name); color: Theme.warning; wrap: word-wrap; } BodyText { text: GuiI18n.text("gui-delete-choice-detail"); wrap: word-wrap; } if root.modal == "delete-instance" && root.pending-action-linked-servers > 0 : BodyText { text: GuiI18n.count("gui-delete-instance-linked", root.pending-action-linked-servers); color: Theme.warning; wrap: word-wrap; } if root.modal-error != "" : BodyText { text: root.modal-error; color: Theme.danger; wrap: word-wrap; } ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-cancel"); clicked => { root.modal = ""; } } AppButton { text: GuiI18n.text("gui-remove-from-ephervex"); clicked => { if root.modal == "delete-instance" { root.delete-client(root.pending-action-id, false); } else { root.delete-server(root.pending-action-id, false); } } } AppButton { text: GuiI18n.text("gui-delete-files"); tone: "danger"; clicked => { if root.modal == "delete-instance" { root.delete-client(root.pending-action-id, true); } else { root.delete-server(root.pending-action-id, true); } } } } }
                    if root.modal == "license" : ScrollView { Text { text: root.license-text; color: Theme.text-secondary; font-family: Theme.mono-family; font-size: 12px; wrap: word-wrap; } }
                    if root.modal == "third-party" : VerticalLayout { spacing: 12px; BodyText { text: GuiI18n.text("gui-third-party-unavailable"); wrap: word-wrap; } BodyText { text: GuiI18n.text("gui-slint-attribution"); color: Theme.text-muted; wrap: word-wrap; } ActionRow { Rectangle { horizontal-stretch: 1; } AppButton { text: GuiI18n.text("gui-close"); tone: "primary"; clicked => { root.modal = ""; } } } }
                }
            }
        }
    }
}
