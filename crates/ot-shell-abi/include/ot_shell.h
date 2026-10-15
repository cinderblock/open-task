/*
 * The boundary between open-task and a toolkit plugin on Linux.
 *
 * `open-task` links no GUI toolkit. A plugin (GTK 4, Qt 6) is a shared object
 * that it loads at run time and finds through one symbol:
 *
 *     const OtPlugin *ot_shell_plugin(void);
 *
 * The plugin owns the main loop and the one window; the host (open-task) owns the
 * app. The plugin reports input as OtEvent and asks the host for a frame as an
 * OtDisplayList, which it draws with the toolkit's own 2D primitives. The host
 * calls the plugin's functions to change the window and to ask the user things.
 *
 * Rules both sides keep:
 * - Everything happens on the plugin's UI thread, except OtPlugin.wake, which
 *   any thread may call.
 * - No plugin function blocks on the user or runs a nested main loop. A menu, a
 *   message, a question or a dialog is shown and the call returns; the answer
 *   comes back later through the OtHost callback named for it, with the token
 *   the host passed.
 * - Strings are UTF-8, passed as pointer and length, not NUL-terminated, and
 *   valid only for the duration of the call. Copy what you keep.
 * - No exception or Rust panic crosses the boundary.
 * - Units are DIPs (device-independent pixels, 1/96 inch at scale 1). The
 *   plugin applies the window's scale.
 *
 * The draw types mirror crates/ot-paint exactly (it is #[repr(C)]); the static
 * assertions at the end of this file and in crates/ot-shell-abi/src/lib.rs check
 * that the two agree. Change both together, and bump OT_SHELL_ABI_VERSION.
 */
#ifndef OT_SHELL_H
#define OT_SHELL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OT_SHELL_ABI_VERSION 2u

/* ---- Drawing: the layout of crates/ot-paint ---------------------------- */

typedef struct OtPoint { float x, y; } OtPoint;
typedef struct OtRect { float x, y, w, h; } OtRect;
/* Straight (not premultiplied) alpha, components 0..1, sRGB. */
typedef struct OtColor { float r, g, b, a; } OtColor;
/* A range in one of the list's arenas. */
typedef struct OtSpan { uint32_t start, len; } OtSpan;

enum OtFontFamily { OT_FONT_UI = 0, OT_FONT_MONO = 1, OT_FONT_ICONS = 2 };
enum OtFontWeight { OT_WEIGHT_REGULAR = 0, OT_WEIGHT_SEMIBOLD = 1, OT_WEIGHT_BOLD = 2 };
enum OtHAlign { OT_HALIGN_LEFT = 0, OT_HALIGN_CENTER = 1, OT_HALIGN_RIGHT = 2 };
enum OtVAlign { OT_VALIGN_TOP = 0, OT_VALIGN_MIDDLE = 1, OT_VALIGN_BOTTOM = 2 };

/* Symbolic icons, by meaning; the plugin maps them to its icon theme. */
enum OtIcon {
    OT_ICON_MENU = 0,
    OT_ICON_SUMMARY,
    OT_ICON_PROCESSES,
    OT_ICON_PERFORMANCE,
    OT_ICON_SETTINGS,
    OT_ICON_UPDATE,
    OT_ICON_USERS,
    OT_ICON_SERVICES,
    OT_ICON_STARTUP,
    OT_ICON_CONNECTIONS,
    OT_ICON_APPS,
    OT_ICON_SYSTEM,
    OT_ICON_PLAY,
    OT_ICON_PAUSE,
    OT_ICON_PREVIOUS,
    OT_ICON_NEXT,
    /* The process menu's entries. */
    OT_ICON_TREE,
    OT_ICON_END_TASK,
    OT_ICON_END_TREE,
    OT_ICON_RESTART,
    OT_ICON_EFFICIENCY,
    OT_ICON_PRIORITY,
    OT_ICON_AFFINITY,
    OT_ICON_SWITCH_TO,
    OT_ICON_OPEN_FOLDER,
    OT_ICON_SEARCH_ONLINE,
    OT_ICON_PROPERTIES,
    OT_ICON_COPY,
    OT_ICON_DUMP,
    OT_ICON_WAIT_CHAIN,
    OT_ICON_STOPWATCH,
    OT_ICON_COUNT
};
/* A menu item with no icon. */
#define OT_ICON_NONE 0xFFFFFFFFu

typedef struct OtTextStyle {
    uint32_t family; /* OtFontFamily */
    float size;      /* DIPs */
    uint32_t weight; /* OtFontWeight */
    bool tabular_numbers;
} OtTextStyle;

typedef struct OtTextCmd {
    OtSpan text; /* in the string arena */
    OtRect rect;
    OtTextStyle style;
    OtColor color;
    uint32_t halign; /* OtHAlign */
    uint32_t valign; /* OtVAlign */
    /* Trim with an ellipsis when wider than rect. Text is always clipped to rect. */
    bool ellipsis;
    /* An editable field: never trimmed; when wider than rect, shifted left so
     * its end stays visible. */
    bool field;
    /* Draw an insertion caret after the last character, in the text color. */
    bool caret;
} OtTextCmd;

enum OtCmdTag {
    OT_CMD_CLEAR = 0,
    OT_CMD_FILL_RECT,
    OT_CMD_FILL_ROUND_RECT,
    OT_CMD_STROKE_RECT,
    OT_CMD_STROKE_ROUND_RECT,
    OT_CMD_LINE,
    OT_CMD_POLYLINE,
    OT_CMD_FILL_POLYGON,
    /* The region between two lines that are functions of x, each ascending in x,
     * over the x-range both cover. */
    OT_CMD_BAND,
    /* A chart line through points ascending in x. */
    OT_CMD_GRAPH,
    OT_CMD_TEXT,
    OT_CMD_ICON,
    /* A raster image found by its path, scaled into rect. One not loaded yet
     * draws nothing this frame. */
    OT_CMD_IMAGE,
    OT_CMD_PUSH_CLIP,
    OT_CMD_POP_CLIP
};

typedef struct OtDrawCmd {
    uint32_t tag; /* OtCmdTag */
    union {
        struct { OtColor color; } clear;
        struct { OtRect rect; OtColor color; } fill_rect;
        struct { OtRect rect; float radius; OtColor color; } fill_round_rect;
        /* Strokes lie inside rect. */
        struct { OtRect rect; OtColor color; float width; } stroke_rect;
        struct { OtRect rect; float radius; OtColor color; float width; } stroke_round_rect;
        struct { OtPoint from, to; OtColor color; float width; } line;
        struct { OtSpan points; OtColor color; float width; } polyline;
        struct { OtSpan points; OtColor color; } fill_polygon;
        struct { OtSpan top, bottom; OtColor color; } band;
        struct { OtSpan points; OtColor color; float width; } graph;
        OtTextCmd text;
        /* size DIPs tall, centered in rect. */
        struct { uint32_t icon; OtRect rect; float size; OtColor color; } icon;
        struct { OtSpan path; OtRect rect; } image;
        struct { OtRect rect; } push_clip;
    } u;
} OtDrawCmd;

/* One frame. Valid until the plugin next calls into the host. */
typedef struct OtDisplayList {
    const OtDrawCmd *cmds;
    size_t cmd_count;
    const OtPoint *points;
    size_t point_count;
    const char *strings; /* UTF-8; spans index bytes */
    size_t strings_len;
} OtDisplayList;

/* ---- Input: plugin to host ---------------------------------------------- */

enum OtEventKind {
    /* The drawing area's size changed: x, y are its width and height. */
    OT_EVENT_RESIZE = 0,
    /* The window's scale changed: x is DIPs-to-device-pixels. */
    OT_EVENT_SCALE,
    OT_EVENT_MOUSE_MOVE,
    OT_EVENT_MOUSE_LEAVE,
    OT_EVENT_MOUSE_DOWN,
    OT_EVENT_MOUSE_UP,
    /* dx, dy in lines (a wheel notch is 1); positive dy scrolls content down
     * (wheel toward the user), positive dx to the right. */
    OT_EVENT_WHEEL,
    OT_EVENT_KEY,
    /* A printable character was typed: codepoint. */
    OT_EVENT_TEXT,
    /* The user asked for a context menu: at x, y when flag is 1, else from the
     * keyboard. */
    OT_EVENT_CONTEXT_MENU,
    /* The window's close button. The host decides; it calls OtPlugin.close or
     * OtPlugin.hide. */
    OT_EVENT_CLOSE_REQUEST,
    /* flag: 1 the window became active, 0 it lost focus. */
    OT_EVENT_FOCUS,
    /* flag: 1 minimized (or hidden by the compositor), 0 shown again. */
    OT_EVENT_VISIBILITY,
    /* The desktop's colour scheme changed: flag 1 dark, 0 light. Plugins that
     * cannot tell never send it; the host also follows the XDG portal. */
    OT_EVENT_THEME
};

enum OtMouseButton { OT_BUTTON_LEFT = 0, OT_BUTTON_RIGHT = 1, OT_BUTTON_MIDDLE = 2 };

enum OtKey {
    OT_KEY_NONE = 0,
    OT_KEY_UP,
    OT_KEY_DOWN,
    OT_KEY_PAGE_UP,
    OT_KEY_PAGE_DOWN,
    OT_KEY_HOME,
    OT_KEY_END,
    OT_KEY_LEFT,
    OT_KEY_RIGHT,
    OT_KEY_BACKSPACE,
    OT_KEY_DELETE,
    OT_KEY_ESCAPE,
    OT_KEY_ENTER,
    OT_KEY_TAB,
    OT_KEY_MENU,
    /* A letter or digit key, for shortcuts: codepoint holds it in lower case
     * ('t', '1'), as the key is labelled without Shift. */
    OT_KEY_CHARACTER
};

enum OtModifier {
    OT_MOD_CTRL = 1,
    OT_MOD_SHIFT = 2,
    OT_MOD_ALT = 4,
    OT_MOD_SUPER = 8
};

typedef struct OtEvent {
    uint32_t kind;      /* OtEventKind */
    uint32_t button;    /* OtMouseButton */
    uint32_t key;       /* OtKey */
    uint32_t modifiers; /* OtModifier bits */
    uint32_t codepoint;
    uint32_t flag;
    float x, y;
    float dx, dy;
} OtEvent;

/* ---- Menus -------------------------------------------------------------- */

enum OtMenuKind {
    OT_MENU_ITEM = 0,
    /* Starts a submenu labelled `label`; the items up to the matching
     * OT_MENU_SUBMENU_END are in it. */
    OT_MENU_SUBMENU,
    OT_MENU_SUBMENU_END,
    OT_MENU_SEPARATOR
};

typedef struct OtMenuItem {
    uint32_t kind; /* OtMenuKind */
    bool enabled;
    bool checked;
    uint64_t id; /* what OtHost.menu_chosen reports for an item */
    const char *label;
    size_t label_len;
    uint32_t icon; /* OtIcon, or OT_ICON_NONE */
} OtMenuItem;

/* ---- The host's callbacks ----------------------------------------------- */

enum OtNoticeKind { OT_NOTICE_INFO = 0, OT_NOTICE_WARNING = 1, OT_NOTICE_ERROR = 2 };
enum OtCursor { OT_CURSOR_ARROW = 0, OT_CURSOR_RESIZE_COLUMN = 1, OT_CURSOR_TEXT = 2, OT_CURSOR_CROSSHAIR = 3 };
enum OtLogLevel { OT_LOG_ERROR = 0, OT_LOG_WARN = 1, OT_LOG_INFO = 2, OT_LOG_DEBUG = 3 };

typedef struct OtHost {
    void *ctx;
    void (*event)(void *ctx, const OtEvent *event);
    /* Fill *out with the next frame. */
    void (*paint)(void *ctx, OtDisplayList *out);
    /* Whether something is moving: the plugin asks for another frame at the
     * display's rate while this is true, and stops when it is not. */
    bool (*animating)(void *ctx);
    /* OtPlugin.wake was called; this runs on the UI thread. */
    void (*wake)(void *ctx);
    /* A timer set with OtPlugin.set_timer fired. */
    void (*timer)(void *ctx, uint32_t id);
    /* The menu shown with `token` closed: the chosen item's id, or `chosen`
     * false when dismissed. */
    void (*menu_chosen)(void *ctx, uint64_t token, bool chosen, uint64_t id);
    void (*confirmed)(void *ctx, uint64_t token, bool yes);
    /* A save dialog closed: the path, or NULL when cancelled. */
    void (*file_chosen)(void *ctx, uint64_t token, const char *path, size_t path_len);
    /* The Run dialog closed: the command line, or NULL when cancelled;
     * `elevated` when the user asked to run it as administrator. */
    void (*run_entered)(void *ctx, uint64_t token, const char *command, size_t command_len,
                        bool elevated);
    void (*log)(void *ctx, uint32_t level, const char *message, size_t message_len);
} OtHost;

/* ---- The plugin --------------------------------------------------------- */

typedef struct OtStartup {
    const char *title;
    size_t title_len;
    /* The reverse-DNS application id, for the desktop entry and the
     * compositor ("io.github.cinderblock.open-task"). */
    const char *app_id;
    size_t app_id_len;
    /* The window's size in DIPs; 0 for the plugin's default. */
    float width, height;
    bool maximized;
    /* The theme the app starts in, for the window's own chrome. */
    bool dark;
    /* argv as the process got it, for toolkits that parse their options. */
    int argc;
    char **argv;
} OtStartup;

typedef struct OtPlugin {
    uint32_t abi_version; /* OT_SHELL_ABI_VERSION */
    uint32_t struct_size; /* sizeof(OtPlugin) */
    /* "GTK 4.14.5", "Qt 6.4.2": the toolkit as it runs, for the log. */
    const char *(*toolkit)(void);
    /* Connect to the display and open the window, then run the main loop until
     * the window closes. Returns 0, or on failure to start writes why into
     * `error` (NUL-terminated, at most error_cap bytes) and returns nonzero, in
     * which case the host may try another plugin. */
    int32_t (*run)(const OtHost *host, const OtStartup *startup, char *error, size_t error_cap);
    /* Any thread: call OtHost.wake on the UI thread soon. Calls made before the
     * host's wake runs may be coalesced into one. */
    void (*wake)(void);
    /* Draw a frame soon. */
    void (*invalidate)(void);
    void (*set_title)(const char *title, size_t title_len);
    void (*set_cursor)(uint32_t cursor);           /* OtCursor */
    void (*set_keep_above)(bool on);               /* best effort; Wayland may refuse */
    void (*set_dark)(bool dark);                   /* the window chrome's theme */
    void (*present)(void);                         /* show, unminimize, raise */
    void (*minimize)(void);
    void (*hide)(void);
    /* Close the window; run() returns. */
    void (*close)(void);
    /* One-shot timer; setting an id again replaces it. */
    void (*set_timer)(uint32_t id, uint32_t ms);
    void (*show_menu)(uint64_t token, float x, float y, const OtMenuItem *items, size_t count);
    void (*message)(uint32_t kind, const char *text, size_t text_len); /* OtNoticeKind */
    /* A yes/no question, No the default. */
    void (*confirm)(uint64_t token, const char *question, size_t question_len);
    /* A save dialog for one file type. */
    void (*save_file)(uint64_t token, const char *title, size_t title_len,
                      const char *default_name, size_t default_name_len,
                      const char *filter_name, size_t filter_name_len,
                      const char *pattern, size_t pattern_len);
    /* The Run dialog: a command line, filled in with `initial`, and whether to
     * run it as administrator. */
    void (*run_dialog)(uint64_t token, const char *initial, size_t initial_len);
    void (*copy_text)(const char *text, size_t text_len);
} OtPlugin;

typedef const OtPlugin *(*OtShellPluginFn)(void);
#define OT_SHELL_PLUGIN_SYMBOL "ot_shell_plugin"

/* ---- Layout checks (keep in step with crates/ot-shell-abi/src/lib.rs) ---- */

#if defined(__cplusplus)
#define OT_STATIC_ASSERT(c, m) static_assert(c, m)
#else
#define OT_STATIC_ASSERT(c, m) _Static_assert(c, m)
#endif
OT_STATIC_ASSERT(sizeof(OtPoint) == 8, "OtPoint");
OT_STATIC_ASSERT(sizeof(OtRect) == 16, "OtRect");
OT_STATIC_ASSERT(sizeof(OtColor) == 16, "OtColor");
OT_STATIC_ASSERT(sizeof(OtSpan) == 8, "OtSpan");
OT_STATIC_ASSERT(sizeof(OtTextStyle) == 16, "OtTextStyle");
OT_STATIC_ASSERT(sizeof(OtTextCmd) == 68, "OtTextCmd");
OT_STATIC_ASSERT(offsetof(OtTextCmd, color) == 40, "OtTextCmd.color");
OT_STATIC_ASSERT(offsetof(OtTextCmd, caret) == 66, "OtTextCmd.caret");
OT_STATIC_ASSERT(sizeof(OtDrawCmd) == 72, "OtDrawCmd");
OT_STATIC_ASSERT(offsetof(OtDrawCmd, u) == 4, "OtDrawCmd.u");
OT_STATIC_ASSERT(sizeof(OtEvent) == 40, "OtEvent");
#undef OT_STATIC_ASSERT

#ifdef __cplusplus
}
#endif

#endif /* OT_SHELL_H */
