// Qt 6 plugin for open-task's Linux shell.
//
// Speaks the C ABI in crates/ot-shell-abi/include/ot_shell.h. One window, one
// widget (Canvas) that asks the host for a frame and draws it with QPainter;
// input becomes the ABI's events; menus and dialogs are shown without blocking
// (QMenu::popup, QDialog::open) and answer through the host's callbacks.
//
// Built against the oldest supported Qt (6.2): nothing newer is called, and Qt
// keeps binary compatibility within a major version.

#include "ot_shell.h"

#include <QApplication>
#include <QCheckBox>
#include <QClipboard>
#include <QContextMenuEvent>
#include <QDialog>
#include <QDialogButtonBox>
#include <QDir>
#include <QFileDialog>
#include <QFontDatabase>
#include <QFontMetricsF>
#include <QHash>
#include <QIcon>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QMenu>
#include <QMessageBox>
#include <QMouseEvent>
#include <QPainter>
#include <QPainterPath>
#include <QPointer>
#include <QStaticText>
#include <QStyle>
#include <QTimer>
#include <QVBoxLayout>
#include <QWheelEvent>
#include <QWidget>
#include <QWindow>

#include <atomic>
#include <cmath>
#include <cstring>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

namespace {

// ---- State ---------------------------------------------------------------

OtHost g_host{};
bool g_has_host = false;
std::atomic<bool> g_running{false};
std::atomic<bool> g_pending_wake{false};
std::atomic<bool> g_wake_queued{false};

class Canvas;
QPointer<Canvas> g_canvas;
std::unordered_map<uint32_t, QPointer<QTimer>> g_timers;
// The process's argv as Qt was given it; QApplication keeps references.
int g_argc = 0;
std::vector<std::string> g_argv_store;
std::vector<char *> g_argv;

QString text(const char *p, size_t n) {
    return p ? QString::fromUtf8(p, static_cast<qsizetype>(n)) : QString();
}

void send(const OtEvent &ev) {
    if (g_has_host) g_host.event(g_host.ctx, &ev);
}

OtEvent make(uint32_t kind) {
    OtEvent e{};
    e.kind = kind;
    return e;
}

void log(uint32_t level, const QString &message) {
    if (!g_has_host) return;
    const QByteArray b = message.toUtf8();
    g_host.log(g_host.ctx, level, b.constData(), static_cast<size_t>(b.size()));
}

uint32_t modifiers(Qt::KeyboardModifiers m) {
    uint32_t r = 0;
    if (m & Qt::ControlModifier) r |= OT_MOD_CTRL;
    if (m & Qt::ShiftModifier) r |= OT_MOD_SHIFT;
    if (m & Qt::AltModifier) r |= OT_MOD_ALT;
    if (m & Qt::MetaModifier) r |= OT_MOD_SUPER;
    return r;
}

QColor color(const OtColor &c) { return QColor::fromRgbF(c.r, c.g, c.b, c.a); }

QRectF rectf(const OtRect &r) { return QRectF(r.x, r.y, std::max(0.0f, r.w), std::max(0.0f, r.h)); }

// The freedesktop symbolic icons for each meaning, best first; KDE's Breeze
// names some differently.
const std::vector<const char *> &icon_names(uint32_t icon) {
    static const std::vector<std::vector<const char *>> names = {
        {"open-menu-symbolic", "application-menu", "open-menu"},
        {"view-grid-symbolic", "view-app-grid-symbolic", "view-grid"},
        {"view-list-symbolic", "view-list-details", "view-list"},
        {"utilities-system-monitor-symbolic", "utilities-system-monitor", "power-profile-performance-symbolic"},
        {"emblem-system-symbolic", "configure", "preferences-system"},
        {"view-refresh-symbolic", "view-refresh"},
        {"system-users-symbolic", "system-users"},
        {"applications-system-symbolic", "preferences-system"},
        {"system-shutdown-symbolic", "system-shutdown"},
        {"network-workgroup-symbolic", "network-workgroup", "network-wired"},
        {"applications-other-symbolic", "view-app-grid-symbolic", "applications-other"},
        {"computer-symbolic", "computer"},
        {"media-playback-start-symbolic", "media-playback-start"},
        {"media-playback-pause-symbolic", "media-playback-pause"},
        {"media-skip-backward-symbolic", "media-skip-backward"},
        {"media-skip-forward-symbolic", "media-skip-forward"},
        {"view-list-tree-symbolic", "view-list-tree", "view-list-symbolic"},
        {"window-close-symbolic", "process-stop", "window-close"},
        {"process-stop-symbolic", "process-stop", "edit-delete"},
        {"view-refresh-symbolic", "view-refresh"},
        {"power-profile-power-saver-symbolic", "battery-profile-powersave", "battery"},
        {"power-profile-balanced-symbolic", "speedometer", "power-profile-performance-symbolic"},
        {"application-x-firmware-symbolic", "cpu", "computer"},
        {"go-jump-symbolic", "go-jump", "window-new"},
        {"folder-open-symbolic", "document-open-folder", "folder-open"},
        {"system-search-symbolic", "edit-find", "system-search"},
        {"document-properties-symbolic", "document-properties", "dialog-information"},
        {"edit-copy-symbolic", "edit-copy"},
        {"document-save-symbolic", "document-save"},
        {"insert-link-symbolic", "insert-link"},
        {"alarm-symbolic", "chronometer", "preferences-system-time"},
    };
    static const std::vector<const char *> none;
    return icon < names.size() ? names[icon] : none;
}

// `src` in one colour: freedesktop symbolic icons are single-colour, and outside
// KDE nothing recolours them for Qt.
QPixmap tint(const QPixmap &src, const QColor &c) {
    QImage img = src.toImage().convertToFormat(QImage::Format_ARGB32_Premultiplied);
    QPainter tp(&img);
    tp.setCompositionMode(QPainter::CompositionMode_SourceIn);
    tp.fillRect(img.rect(), c);
    tp.end();
    QPixmap out = QPixmap::fromImage(img);
    out.setDevicePixelRatio(src.devicePixelRatio());
    return out;
}

// ---- Drawing ---------------------------------------------------------------

struct CachedText {
    QStaticText text;
    QFont font;
    double width = 0;
    double height = 0;
    uint64_t last_used = 0;
};

class Painter {
public:
    void draw(QPainter &p, const OtDisplayList &list, const QWidget *widget) {
        ++frame_;
        p.setRenderHint(QPainter::Antialiasing, true);
        p.setRenderHint(QPainter::TextAntialiasing, true);
        const OtDrawCmd *cmds = list.cmds;
        int clips = 0;
        for (size_t i = 0; i < list.cmd_count; ++i) {
            const OtDrawCmd &c = cmds[i];
            switch (c.tag) {
            case OT_CMD_CLEAR: {
                p.save();
                p.setCompositionMode(QPainter::CompositionMode_Source);
                p.fillRect(widget->rect(), color(c.u.clear.color));
                p.restore();
                break;
            }
            case OT_CMD_FILL_RECT:
                p.fillRect(rectf(c.u.fill_rect.rect), color(c.u.fill_rect.color));
                break;
            case OT_CMD_FILL_ROUND_RECT: {
                const auto &f = c.u.fill_round_rect;
                p.setPen(Qt::NoPen);
                p.setBrush(color(f.color));
                p.drawRoundedRect(rectf(f.rect), f.radius, f.radius);
                break;
            }
            case OT_CMD_STROKE_RECT: {
                const auto &s = c.u.stroke_rect;
                stroke(p, s.rect, 0, s.color, s.width);
                break;
            }
            case OT_CMD_STROKE_ROUND_RECT: {
                const auto &s = c.u.stroke_round_rect;
                stroke(p, s.rect, s.radius, s.color, s.width);
                break;
            }
            case OT_CMD_LINE: {
                const auto &l = c.u.line;
                p.setPen(pen(l.color, l.width));
                p.drawLine(QPointF(l.from.x, l.from.y), QPointF(l.to.x, l.to.y));
                break;
            }
            case OT_CMD_POLYLINE:
            case OT_CMD_GRAPH: {
                const OtSpan s = c.tag == OT_CMD_GRAPH ? c.u.graph.points : c.u.polyline.points;
                const OtColor col = c.tag == OT_CMD_GRAPH ? c.u.graph.color : c.u.polyline.color;
                const float w = c.tag == OT_CMD_GRAPH ? c.u.graph.width : c.u.polyline.width;
                const QPolygonF poly = polygon(list, s, false);
                if (poly.size() < 2) break;
                p.setPen(pen(col, w));
                p.setBrush(Qt::NoBrush);
                p.drawPolyline(poly);
                break;
            }
            case OT_CMD_FILL_POLYGON: {
                const QPolygonF poly = polygon(list, c.u.fill_polygon.points, false);
                if (poly.size() < 3) break;
                p.setPen(Qt::NoPen);
                p.setBrush(color(c.u.fill_polygon.color));
                p.drawPolygon(poly);
                break;
            }
            case OT_CMD_BAND: {
                // Along the top, back along the bottom: the region between.
                QPolygonF poly = polygon(list, c.u.band.top, false);
                const QPolygonF bottom = polygon(list, c.u.band.bottom, true);
                if (poly.size() < 2 || bottom.size() < 2) break;
                poly += bottom;
                p.setPen(Qt::NoPen);
                p.setBrush(color(c.u.band.color));
                p.drawPolygon(poly);
                break;
            }
            case OT_CMD_TEXT:
                drawText(p, list, c.u.text, widget);
                break;
            case OT_CMD_ICON:
                drawIcon(p, c.u.icon.icon, c.u.icon.rect, c.u.icon.size, c.u.icon.color, widget);
                break;
            case OT_CMD_IMAGE:
                // Program icons are not loaded on Linux yet.
                break;
            case OT_CMD_PUSH_CLIP:
                p.save();
                p.setClipRect(rectf(c.u.push_clip.rect), Qt::IntersectClip);
                ++clips;
                break;
            case OT_CMD_POP_CLIP:
                if (clips > 0) {
                    p.restore();
                    --clips;
                }
                break;
            default:
                break;
            }
        }
        while (clips-- > 0) p.restore();
        if (frame_ % 120 == 0) evict();
    }

private:
    static QPen pen(const OtColor &c, float width) {
        QPen pen(color(c));
        pen.setWidthF(width);
        pen.setCapStyle(Qt::FlatCap);
        pen.setJoinStyle(Qt::RoundJoin);
        return pen;
    }

    // An outline inside rect, as Direct2D's and GSK's are drawn.
    static void stroke(QPainter &p, const OtRect &r, float radius, const OtColor &c, float width) {
        const float h = width / 2;
        const QRectF inner(r.x + h, r.y + h, std::max(0.0f, r.w - width), std::max(0.0f, r.h - width));
        p.setPen(pen(c, width));
        p.setBrush(Qt::NoBrush);
        if (radius > 0) {
            const float rr = std::max(0.0f, radius - h);
            p.drawRoundedRect(inner, rr, rr);
        } else {
            p.drawRect(inner);
        }
    }

    static QPolygonF polygon(const OtDisplayList &list, OtSpan s, bool reversed) {
        QPolygonF poly;
        if (!list.points || static_cast<size_t>(s.start) + s.len > list.point_count) return poly;
        poly.reserve(static_cast<int>(s.len));
        for (uint32_t k = 0; k < s.len; ++k) {
            const OtPoint &pt = list.points[s.start + (reversed ? s.len - 1 - k : k)];
            poly.append(QPointF(pt.x, pt.y));
        }
        return poly;
    }

    QFont fontFor(const OtTextStyle &style) {
        QFont f = style.family == OT_FONT_MONO ? QFontDatabase::systemFont(QFontDatabase::FixedFont)
                                               : QApplication::font();
        // DIPs are Qt's logical pixels.
        f.setPixelSize(std::max(1, static_cast<int>(std::lround(style.size))));
        f.setWeight(style.weight == OT_WEIGHT_BOLD       ? QFont::Bold
                    : style.weight == OT_WEIGHT_SEMIBOLD ? QFont::DemiBold
                                                         : QFont::Normal);
        return f;
    }

    CachedText &layout(const QString &str, const OtTextCmd &t) {
        QByteArray key = str.toUtf8();
        key.append(reinterpret_cast<const char *>(&t.style), sizeof t.style);
        key.append(static_cast<char>(t.ellipsis && !t.field));
        if (t.ellipsis && !t.field) key.append(reinterpret_cast<const char *>(&t.rect.w), sizeof t.rect.w);
        auto it = texts_.find(key);
        if (it == texts_.end()) {
            CachedText c;
            c.font = fontFor(t.style);
            const QFontMetricsF m(c.font);
            const QString shown = (t.ellipsis && !t.field) ? m.elidedText(str, Qt::ElideRight, t.rect.w) : str;
            c.text = QStaticText(shown);
            c.text.setTextFormat(Qt::PlainText);
            c.text.setPerformanceHint(QStaticText::AggressiveCaching);
            c.text.prepare(QTransform(), c.font);
            c.width = m.horizontalAdvance(shown);
            c.height = m.height();
            it = texts_.insert(key, c);
        }
        it->last_used = frame_;
        return *it;
    }

    void drawText(QPainter &p, const OtDisplayList &list, const OtTextCmd &t, const QWidget *) {
        if (!list.strings || static_cast<size_t>(t.text.start) + t.text.len > list.strings_len) return;
        const QString str = QString::fromUtf8(list.strings + t.text.start, static_cast<qsizetype>(t.text.len));
        if (str.isEmpty() && !t.caret) return;
        CachedText &c = layout(str, t);
        const QRectF r = rectf(t.rect);
        double shift = 0;
        double left;
        if (t.field) {
            shift = std::max(0.0, c.width - r.width());
            left = r.x() - shift;
        } else if (t.halign == OT_HALIGN_CENTER) {
            left = r.x() + (r.width() - c.width) / 2;
        } else if (t.halign == OT_HALIGN_RIGHT) {
            left = r.x() + r.width() - c.width;
        } else {
            left = r.x();
        }
        double top = t.valign == OT_VALIGN_TOP      ? r.y()
                     : t.valign == OT_VALIGN_BOTTOM ? r.y() + r.height() - c.height
                                                    : r.y() + (r.height() - c.height) / 2;
        p.save();
        p.setClipRect(r, Qt::IntersectClip);
        p.setFont(c.font);
        p.setPen(color(t.color));
        p.drawStaticText(QPointF(std::round(left), std::round(top)), c.text);
        if (t.caret) {
            p.fillRect(QRectF(std::round(r.x() - shift + c.width), std::round(top), 1.0, c.height), color(t.color));
        }
        p.restore();
    }

    // Theme icons are drawn as masks in the view's colour: freedesktop symbolic
    // icons are single-colour, and outside KDE nothing recolours them for Qt.
    void drawIcon(QPainter &p, uint32_t icon, const OtRect &rect, float size, const OtColor &col,
                  const QWidget *widget) {
        const qreal dpr = widget->devicePixelRatioF();
        const int px = std::max(1, static_cast<int>(std::lround(size)));
        const QColor qc = color(col);
        const QString key = QString::number(icon) + ':' + QString::number(px) + ':' + QString::number(dpr) +
                            ':' + qc.name(QColor::HexArgb);
        auto it = icons_.find(key);
        if (it == icons_.end()) {
            QPixmap tinted;
            for (const char *name : icon_names(icon)) {
                const QIcon i = QIcon::fromTheme(QString::fromLatin1(name));
                if (i.isNull()) continue;
                QPixmap src = i.pixmap(QSize(px, px), dpr);
                if (src.isNull()) continue;
                tinted = tint(src, qc);
                break;
            }
            it = icons_.insert(key, tinted);
        }
        if (it->isNull()) return;
        const QRectF target(rect.x + (rect.w - size) / 2, rect.y + (rect.h - size) / 2, size, size);
        p.drawPixmap(target, *it, QRectF(QPointF(0, 0), QSizeF(it->size())));
    }

    void evict() {
        for (auto it = texts_.begin(); it != texts_.end();) {
            if (frame_ - it->last_used > 120) it = texts_.erase(it);
            else ++it;
        }
        if (icons_.size() > 512) icons_.clear();
    }

    QHash<QByteArray, CachedText> texts_;
    QHash<QString, QPixmap> icons_;
    uint64_t frame_ = 0;
};

// ---- The window --------------------------------------------------------

class Canvas : public QWidget {
public:
    Canvas() {
        setMouseTracking(true);
        setFocusPolicy(Qt::StrongFocus);
        setAttribute(Qt::WA_OpaquePaintEvent);
        setAttribute(Qt::WA_NoSystemBackground);
        frames_.setTimerType(Qt::PreciseTimer);
        frames_.setInterval(16);
        QObject::connect(&frames_, &QTimer::timeout, this, [this] { tick(); });
    }

    bool allow_close = false;

protected:
    void paintEvent(QPaintEvent *) override {
        if (!g_has_host) return;
        OtDisplayList list{};
        g_host.paint(g_host.ctx, &list);
        QPainter p(this);
        painter_.draw(p, list, this);
        p.end();
        // While something moves, a timer asks for the next frame, at the display's
        // rate when the window is active and half of it in the background.
        if (g_host.animating(g_host.ctx)) {
            frames_.setInterval(isActiveWindow() ? 16 : 33);
            if (!frames_.isActive()) frames_.start();
        }
    }

    void resizeEvent(QResizeEvent *e) override {
        QWidget::resizeEvent(e);
        OtEvent ev = make(OT_EVENT_RESIZE);
        ev.x = static_cast<float>(width());
        ev.y = static_cast<float>(height());
        send(ev);
    }

    void mouseMoveEvent(QMouseEvent *e) override {
        OtEvent ev = make(OT_EVENT_MOUSE_MOVE);
        ev.x = static_cast<float>(e->position().x());
        ev.y = static_cast<float>(e->position().y());
        send(ev);
    }

    void leaveEvent(QEvent *) override { send(make(OT_EVENT_MOUSE_LEAVE)); }

    void mousePressEvent(QMouseEvent *e) override { button(e, OT_EVENT_MOUSE_DOWN); }

    void mouseReleaseEvent(QMouseEvent *e) override {
        // Only the left button's release is the view's; a right click is the
        // context menu, which Qt reports on its own.
        if (e->button() == Qt::LeftButton) button(e, OT_EVENT_MOUSE_UP);
    }

    void mouseDoubleClickEvent(QMouseEvent *e) override { button(e, OT_EVENT_MOUSE_DOWN); }

    void contextMenuEvent(QContextMenuEvent *e) override {
        OtEvent ev = make(OT_EVENT_CONTEXT_MENU);
        if (e->reason() != QContextMenuEvent::Keyboard) {
            ev.flag = 1;
            ev.x = static_cast<float>(e->pos().x());
            ev.y = static_cast<float>(e->pos().y());
        }
        send(ev);
    }

    void wheelEvent(QWheelEvent *e) override {
        // A notch is 120 eighths of a degree; away from the user is positive in
        // Qt and scrolls content up, so it is a negative line count here.
        OtEvent ev = make(OT_EVENT_WHEEL);
        const QPoint d = e->angleDelta();
        ev.dy = -static_cast<float>(d.y()) / 120.0f;
        ev.dx = -static_cast<float>(d.x()) / 120.0f;
        ev.modifiers = modifiers(e->modifiers());
        ev.x = static_cast<float>(e->position().x());
        ev.y = static_cast<float>(e->position().y());
        send(ev);
        e->accept();
    }

    void keyPressEvent(QKeyEvent *e) override {
        OtEvent ev = make(OT_EVENT_KEY);
        ev.modifiers = modifiers(e->modifiers());
        switch (e->key()) {
        case Qt::Key_Up: ev.key = OT_KEY_UP; break;
        case Qt::Key_Down: ev.key = OT_KEY_DOWN; break;
        case Qt::Key_PageUp: ev.key = OT_KEY_PAGE_UP; break;
        case Qt::Key_PageDown: ev.key = OT_KEY_PAGE_DOWN; break;
        case Qt::Key_Home: ev.key = OT_KEY_HOME; break;
        case Qt::Key_End: ev.key = OT_KEY_END; break;
        case Qt::Key_Left: ev.key = OT_KEY_LEFT; break;
        case Qt::Key_Right: ev.key = OT_KEY_RIGHT; break;
        case Qt::Key_Backspace: ev.key = OT_KEY_BACKSPACE; break;
        case Qt::Key_Delete: ev.key = OT_KEY_DELETE; break;
        case Qt::Key_Escape: ev.key = OT_KEY_ESCAPE; break;
        case Qt::Key_Return:
        case Qt::Key_Enter: ev.key = OT_KEY_ENTER; break;
        case Qt::Key_Tab:
        case Qt::Key_Backtab: ev.key = OT_KEY_TAB; break;
        // The menu key comes as a context menu event; Shift+F10, its stand-in, Qt
        // leaves as a key.
        case Qt::Key_F10:
            if (e->modifiers() & Qt::ShiftModifier) ev.key = OT_KEY_MENU;
            break;
        default: break;
        }
        if (ev.key != OT_KEY_NONE) {
            send(ev);
            return;
        }
        const bool shortcut = e->modifiers() & (Qt::ControlModifier | Qt::AltModifier | Qt::MetaModifier);
        const int k = e->key();
        if (shortcut) {
            // A letter or digit with Ctrl: the key as labelled, lower case.
            if ((k >= Qt::Key_A && k <= Qt::Key_Z) || (k >= Qt::Key_0 && k <= Qt::Key_9)) {
                ev.key = OT_KEY_CHARACTER;
                ev.codepoint = static_cast<uint32_t>(QChar(k).toLower().unicode());
                send(ev);
            }
            return;
        }
        const QString t = e->text();
        for (const uint c : t.toUcs4()) {
            if (c < 0x20 || c == 0x7f) continue;
            OtEvent te = make(OT_EVENT_TEXT);
            te.codepoint = c;
            send(te);
        }
    }

    // Tab must reach the view (Ctrl+Tab walks the pages), not move focus.
    bool focusNextPrevChild(bool) override { return false; }

    void changeEvent(QEvent *e) override {
        QWidget::changeEvent(e);
        if (e->type() == QEvent::ActivationChange) {
            OtEvent ev = make(OT_EVENT_FOCUS);
            ev.flag = isActiveWindow() ? 1 : 0;
            send(ev);
        } else if (e->type() == QEvent::WindowStateChange) {
            OtEvent ev = make(OT_EVENT_VISIBILITY);
            ev.flag = isMinimized() ? 1 : 0;
            send(ev);
        }
    }

    void closeEvent(QCloseEvent *e) override {
        if (allow_close) {
            e->accept();
            return;
        }
        // The host decides: it calls close() or hide().
        e->ignore();
        send(make(OT_EVENT_CLOSE_REQUEST));
    }

private:
    void button(QMouseEvent *e, uint32_t kind) {
        OtEvent ev = make(kind);
        switch (e->button()) {
        case Qt::LeftButton: ev.button = OT_BUTTON_LEFT; break;
        case Qt::RightButton: ev.button = OT_BUTTON_RIGHT; break;
        case Qt::MiddleButton: ev.button = OT_BUTTON_MIDDLE; break;
        default: return;
        }
        ev.modifiers = modifiers(e->modifiers());
        ev.x = static_cast<float>(e->position().x());
        ev.y = static_cast<float>(e->position().y());
        send(ev);
    }

    void tick() {
        if (g_has_host && g_host.animating(g_host.ctx)) {
            update();
        } else {
            frames_.stop();
        }
    }

    Painter painter_;
    QTimer frames_;
};

// ---- Dialogs -------------------------------------------------------------

// The heading and the body of a host message, "What happened.\n\nWhy."
void split(const QString &t, QString &head, QString &body) {
    const int i = t.indexOf(QStringLiteral("\n\n"));
    if (i < 0) {
        head = t;
        body.clear();
    } else {
        head = t.left(i);
        body = t.mid(i + 2);
    }
}

QMessageBox *box(QMessageBox::Icon icon, const QString &t) {
    QString head, body;
    split(t, head, body);
    auto *b = new QMessageBox(icon, QStringLiteral("open-task"), head, QMessageBox::NoButton, g_canvas);
    b->setInformativeText(body);
    b->setAttribute(Qt::WA_DeleteOnClose);
    b->setWindowModality(Qt::WindowModal);
    return b;
}

// A menu entry's icon from the theme, in the menu's own text colours for each
// state it is drawn in; none for OT_ICON_NONE or a theme without it.
QIcon menu_icon(uint32_t icon, const QMenu *menu) {
    if (icon == OT_ICON_NONE) return {};
    const int px = menu->style()->pixelMetric(QStyle::PM_SmallIconSize, nullptr, menu);
    for (const char *name : icon_names(icon)) {
        const QIcon themed = QIcon::fromTheme(QString::fromLatin1(name));
        if (themed.isNull()) continue;
        const QPixmap src = themed.pixmap(QSize(px, px), menu->devicePixelRatioF());
        if (src.isNull()) continue;
        const QPalette pal = menu->palette();
        QIcon out;
        out.addPixmap(tint(src, pal.color(QPalette::Active, QPalette::Text)), QIcon::Normal);
        // The highlighted entry.
        out.addPixmap(tint(src, pal.color(QPalette::Active, QPalette::HighlightedText)), QIcon::Active);
        out.addPixmap(tint(src, pal.color(QPalette::Disabled, QPalette::Text)), QIcon::Disabled);
        return out;
    }
    return {};
}

void show_menu(uint64_t token, float x, float y, const OtMenuItem *items, size_t count) {
    if (!g_canvas) return;
    auto *top = new QMenu(g_canvas);
    top->setAttribute(Qt::WA_DeleteOnClose);
    auto answered = std::make_shared<bool>(false);
    std::vector<QMenu *> stack{top};
    for (size_t i = 0; i < count; ++i) {
        const OtMenuItem &it = items[i];
        QMenu *menu = stack.back();
        const QString label = text(it.label, it.label_len);
        switch (it.kind) {
        case OT_MENU_ITEM: {
            QAction *a = menu->addAction(menu_icon(it.icon, menu), label);
            a->setEnabled(it.enabled);
            if (it.checked) {
                a->setCheckable(true);
                a->setChecked(true);
            }
            const uint64_t id = it.id;
            QObject::connect(a, &QAction::triggered, [token, id, answered] {
                *answered = true;
                if (g_has_host) g_host.menu_chosen(g_host.ctx, token, true, id);
            });
            break;
        }
        case OT_MENU_SEPARATOR:
            menu->addSeparator();
            break;
        case OT_MENU_SUBMENU: {
            QMenu *sub = menu->addMenu(menu_icon(it.icon, menu), label);
            sub->setEnabled(it.enabled);
            stack.push_back(sub);
            break;
        }
        case OT_MENU_SUBMENU_END:
            if (stack.size() > 1) stack.pop_back();
            break;
        default:
            break;
        }
    }
    // A chosen item's action fires as the menu hides; look once it has.
    QObject::connect(top, &QMenu::aboutToHide, [token, answered] {
        QTimer::singleShot(0, [token, answered] {
            if (!*answered && g_has_host) g_host.menu_chosen(g_host.ctx, token, false, 0);
        });
    });
    top->popup(g_canvas->mapToGlobal(QPoint(static_cast<int>(std::lround(x)), static_cast<int>(std::lround(y)))));
}

void run_dialog(uint64_t token, const QString &initial) {
    auto *d = new QDialog(g_canvas);
    d->setWindowTitle(QStringLiteral("Run new task"));
    d->setAttribute(Qt::WA_DeleteOnClose);
    d->setWindowModality(Qt::WindowModal);
    auto *layout = new QVBoxLayout(d);
    auto *prompt = new QLabel(QStringLiteral(
        "Type the name of a program, folder, document or internet resource, and open-task will open it for you."));
    prompt->setWordWrap(true);
    auto *entry = new QLineEdit(initial);
    auto *elevated = new QCheckBox(QStringLiteral("Create this task with administrative privileges"));
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel);
    layout->addWidget(prompt);
    layout->addWidget(entry);
    layout->addWidget(elevated);
    layout->addWidget(buttons);
    d->resize(440, d->sizeHint().height());
    QObject::connect(buttons, &QDialogButtonBox::accepted, d, &QDialog::accept);
    QObject::connect(buttons, &QDialogButtonBox::rejected, d, &QDialog::reject);
    QObject::connect(d, &QDialog::finished, [token, entry, elevated](int r) {
        if (!g_has_host) return;
        const QByteArray cmd = entry->text().toUtf8();
        if (r == QDialog::Accepted && !entry->text().trimmed().isEmpty()) {
            g_host.run_entered(g_host.ctx, token, cmd.constData(), static_cast<size_t>(cmd.size()),
                               elevated->isChecked());
        } else {
            g_host.run_entered(g_host.ctx, token, nullptr, 0, false);
        }
    });
    d->open();
    entry->setFocus();
    entry->selectAll();
}

// ---- The function table ------------------------------------------------

const char *toolkit() {
    static const std::string name = std::string("Qt ") + qVersion();
    return name.c_str();
}

void deliver_wake() {
    if (g_wake_queued.exchange(true)) return;
    QMetaObject::invokeMethod(
        qApp,
        [] {
            g_wake_queued.store(false);
            if (g_has_host) g_host.wake(g_host.ctx);
        },
        Qt::QueuedConnection);
}

void write_error(char *error, size_t cap, const std::string &m) {
    if (!error || cap == 0) return;
    const size_t n = std::min(m.size(), cap - 1);
    std::memcpy(error, m.data(), n);
    error[n] = 0;
}

int32_t run(const OtHost *host, const OtStartup *startup, char *error, size_t error_cap) {
    if (!host || !startup) {
        write_error(error, error_cap, "no host table");
        return 2;
    }
    // Qt needs a display; without one QApplication would abort the process.
    if (qEnvironmentVariableIsEmpty("WAYLAND_DISPLAY") && qEnvironmentVariableIsEmpty("DISPLAY") &&
        qEnvironmentVariableIsEmpty("QT_QPA_PLATFORM")) {
        write_error(error, error_cap, "no display");
        return 1;
    }
    g_host = *host;
    g_has_host = true;
    g_argv_store.clear();
    for (int i = 0; i < std::max(1, startup->argc); ++i) {
        g_argv_store.emplace_back(startup->argv && startup->argv[i] ? startup->argv[i] : "open-task");
    }
    g_argv.clear();
    for (auto &s : g_argv_store) g_argv.push_back(s.data());
    g_argv.push_back(nullptr);
    g_argc = static_cast<int>(g_argv_store.size());

    const QString app_id = text(startup->app_id, startup->app_id_len);
    QGuiApplication::setDesktopFileName(app_id);
    QApplication::setApplicationName(QStringLiteral("open-task"));
    QApplication app(g_argc, g_argv.data());
    app.setQuitOnLastWindowClosed(false);
    // Outside a desktop that sets one (KDE: Breeze), Qt has no icon theme and
    // finds no icons at all; use one that is installed.
    if (QIcon::themeName().isEmpty() || QIcon::themeName() == QLatin1String("hicolor")) {
        for (const char *name : {"breeze", "Adwaita"}) {
            bool found = false;
            for (const QString &dir : QIcon::themeSearchPaths()) {
                if (QDir(dir + QLatin1Char('/') + QLatin1String(name)).exists()) {
                    QIcon::setThemeName(QLatin1String(name));
                    found = true;
                    break;
                }
            }
            if (found) break;
        }
    }

    Canvas canvas;
    g_canvas = &canvas;
    canvas.setWindowTitle(text(startup->title, startup->title_len));
    canvas.setWindowIcon(QIcon::fromTheme(app_id));
    if (startup->width > 0 && startup->height > 0) {
        canvas.resize(static_cast<int>(std::lround(startup->width)), static_cast<int>(std::lround(startup->height)));
    }
    log(OT_LOG_INFO, QStringLiteral("%1 window opening on %2")
                         .arg(QString::fromUtf8(toolkit()), QGuiApplication::platformName()));
    g_running.store(true);
    if (g_pending_wake.exchange(false)) deliver_wake();
    if (startup->maximized) canvas.showMaximized();
    else canvas.show();
    canvas.setFocus();
    const int r = app.exec();
    g_running.store(false);
    for (auto &t : g_timers) delete t.second.data();
    g_timers.clear();
    g_canvas = nullptr;
    g_has_host = false;
    (void)r;
    return 0;
}

void wake() {
    if (g_running.load()) deliver_wake();
    else g_pending_wake.store(true);
}

void invalidate() {
    if (g_canvas) g_canvas->update();
}

void set_title(const char *t, size_t n) {
    if (g_canvas) g_canvas->setWindowTitle(text(t, n));
}

void set_cursor(uint32_t c) {
    if (!g_canvas) return;
    switch (c) {
    case OT_CURSOR_RESIZE_COLUMN: g_canvas->setCursor(Qt::SplitHCursor); break;
    case OT_CURSOR_TEXT: g_canvas->setCursor(Qt::IBeamCursor); break;
    case OT_CURSOR_CROSSHAIR: g_canvas->setCursor(Qt::CrossCursor); break;
    default: g_canvas->unsetCursor(); break;
    }
}

void set_keep_above(bool on) {
    if (!g_canvas) return;
    if (g_canvas->windowFlags().testFlag(Qt::WindowStaysOnTopHint) == on) return;
    // Changing a flag re-creates the native window, which hides it.
    const bool visible = g_canvas->isVisible();
    g_canvas->setWindowFlag(Qt::WindowStaysOnTopHint, on);
    if (visible) g_canvas->show();
}

void set_dark(bool) {
    // Qt follows the desktop's colour scheme itself; 6.2 has no API to ask.
}

void present() {
    if (!g_canvas) return;
    g_canvas->show();
    g_canvas->setWindowState((g_canvas->windowState() & ~Qt::WindowMinimized) | Qt::WindowActive);
    g_canvas->raise();
    g_canvas->activateWindow();
}

void minimize() {
    if (g_canvas) g_canvas->showMinimized();
}

void hide() {
    if (g_canvas) g_canvas->hide();
}

void close() {
    if (!g_canvas) return;
    g_canvas->allow_close = true;
    g_canvas->close();
    QCoreApplication::quit();
}

void set_timer(uint32_t id, uint32_t ms) {
    auto &slot = g_timers[id];
    if (!slot) {
        slot = new QTimer(qApp);
        slot->setSingleShot(true);
        QObject::connect(slot.data(), &QTimer::timeout, [id] {
            if (g_has_host) g_host.timer(g_host.ctx, id);
        });
    }
    slot->start(static_cast<int>(std::min<uint32_t>(ms, INT32_MAX)));
}

void show_menu_c(uint64_t token, float x, float y, const OtMenuItem *items, size_t count) {
    show_menu(token, x, y, items, count);
}

void message(uint32_t kind, const char *t, size_t n) {
    const auto icon = kind == OT_NOTICE_INFO    ? QMessageBox::Information
                      : kind == OT_NOTICE_ERROR ? QMessageBox::Critical
                                                : QMessageBox::Warning;
    QMessageBox *b = box(icon, text(t, n));
    b->setStandardButtons(QMessageBox::Ok);
    b->open();
}

void confirm(uint64_t token, const char *q, size_t n) {
    QMessageBox *b = box(QMessageBox::Warning, text(q, n));
    b->setStandardButtons(QMessageBox::Yes | QMessageBox::No);
    b->setDefaultButton(QMessageBox::No);
    b->setEscapeButton(QMessageBox::No);
    QObject::connect(b, &QMessageBox::finished, [token, b](int) {
        const bool yes = b->clickedButton() == b->button(QMessageBox::Yes);
        if (g_has_host) g_host.confirmed(g_host.ctx, token, yes);
    });
    b->open();
}

void save_file(uint64_t token, const char *title, size_t title_len, const char *name, size_t name_len,
               const char *filter, size_t filter_len, const char *pattern, size_t pattern_len) {
    auto *d = new QFileDialog(g_canvas, text(title, title_len));
    d->setAttribute(Qt::WA_DeleteOnClose);
    d->setAcceptMode(QFileDialog::AcceptSave);
    d->setFileMode(QFileDialog::AnyFile);
    d->setNameFilter(QStringLiteral("%1 (%2)").arg(text(filter, filter_len), text(pattern, pattern_len)));
    d->selectFile(text(name, name_len));
    auto answered = std::make_shared<bool>(false);
    QObject::connect(d, &QFileDialog::fileSelected, [token, answered](const QString &f) {
        *answered = true;
        const QByteArray b = f.toUtf8();
        if (g_has_host) g_host.file_chosen(g_host.ctx, token, b.constData(), static_cast<size_t>(b.size()));
    });
    QObject::connect(d, &QDialog::finished, [token, answered](int) {
        if (!*answered && g_has_host) g_host.file_chosen(g_host.ctx, token, nullptr, 0);
    });
    d->open();
}

void run_dialog_c(uint64_t token, const char *initial, size_t n) { run_dialog(token, text(initial, n)); }

void copy_text(const char *t, size_t n) { QGuiApplication::clipboard()->setText(text(t, n)); }

const OtPlugin PLUGIN = {
    OT_SHELL_ABI_VERSION,
    sizeof(OtPlugin),
    toolkit,
    run,
    wake,
    invalidate,
    set_title,
    set_cursor,
    set_keep_above,
    set_dark,
    present,
    minimize,
    hide,
    close,
    set_timer,
    show_menu_c,
    message,
    confirm,
    save_file,
    run_dialog_c,
    copy_text,
};

} // namespace

extern "C" const OtPlugin *ot_qt_plugin(void) { return &PLUGIN; }
