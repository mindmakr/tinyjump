package com.mindmakr.tinyjump;

import android.content.Context;
import android.content.res.Configuration;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.Rect;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.text.Editable;
import android.text.InputType;
import android.text.TextUtils;
import android.text.TextWatcher;
import android.util.Log;
import android.util.TypedValue;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.ViewTreeObserver;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.CompoundButton;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.net.URL;
import java.text.NumberFormat;
import java.util.ArrayList;
import java.util.Currency;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * Applies view-tree patches to native Views and places them where the core's layout says
 * (crates/tj-core/src/ui/layout.rs). Mirrors the catalog in crates/tj-core/src/ui/catalog.rs.
 */
final class Renderer {
    /** Where a view goes inside a {@link FrameBox}, in pixels. */
    static final class Frame extends ViewGroup.LayoutParams {
        int x;
        int y;

        Frame() {
            super(0, 0);
        }
    }

    /** A container that places children exactly at their frames. */
    static final class FrameBox extends ViewGroup {
        /** Content size in pixels when this box scrolls; 0 otherwise. */
        int contentWidth;
        int contentHeight;

        FrameBox(Context c) {
            super(c);
        }

        @Override
        protected void onMeasure(int widthSpec, int heightSpec) {
            for (int i = 0; i < getChildCount(); i++) {
                View child = getChildAt(i);
                Frame f = (Frame) child.getLayoutParams();
                child.measure(MeasureSpec.makeMeasureSpec(f.width, MeasureSpec.EXACTLY), MeasureSpec.makeMeasureSpec(f.height, MeasureSpec.EXACTLY));
            }
            int w = MeasureSpec.getMode(widthSpec) == MeasureSpec.EXACTLY ? MeasureSpec.getSize(widthSpec) : contentWidth;
            int h = MeasureSpec.getMode(heightSpec) == MeasureSpec.EXACTLY ? MeasureSpec.getSize(heightSpec) : contentHeight;
            setMeasuredDimension(w, h);
        }

        @Override
        protected void onLayout(boolean changed, int l, int t, int r, int b) {
            for (int i = 0; i < getChildCount(); i++) {
                View child = getChildAt(i);
                Frame f = (Frame) child.getLayoutParams();
                child.layout(f.x, f.y, f.x + f.width, f.y + f.height);
            }
        }

        @Override
        protected LayoutParams generateDefaultLayoutParams() {
            return new Frame();
        }

        @Override
        protected boolean checkLayoutParams(LayoutParams p) {
            return p instanceof Frame;
        }

        @Override
        protected LayoutParams generateLayoutParams(LayoutParams p) {
            return new Frame();
        }
    }

    /** Vector stroke icon drawn directly on Canvas in a 24x24 coordinate space. */
    static final class IconView extends View {
        private final float density;
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path path = new Path();
        private String name = "";
        private float sizeDp = 20f;

        IconView(Context ctx, float density) {
            super(ctx);
            this.density = density;
            paint.setStyle(Paint.Style.STROKE);
            paint.setStrokeCap(Paint.Cap.ROUND);
            paint.setStrokeJoin(Paint.Join.ROUND);
            paint.setColor(0xFF111827);
        }

        void setIconName(String name) {
            this.name = name == null ? "" : name;
            invalidate();
        }

        void setIconSize(float dp) {
            this.sizeDp = dp > 0 ? dp : 20f;
            requestLayout();
            invalidate();
        }

        void setIconColor(int color) {
            paint.setColor(color);
            invalidate();
        }

        @Override
        protected void onMeasure(int widthSpec, int heightSpec) {
            int s = Math.max(1, Math.round(sizeDp * density));
            setMeasuredDimension(resolveSize(s, widthSpec), resolveSize(s, heightSpec));
        }

        @Override
        protected void onDraw(Canvas c) {
            super.onDraw(c);
            float s = Math.min(getWidth(), getHeight()) / 24f;
            if (s <= 0f) return;
            paint.setStrokeWidth(2f * s);
            Path p = path;
            p.reset();
            switch (name) {
                case "check":
                    p.moveTo(5 * s, 13 * s); p.lineTo(9 * s, 17 * s); p.lineTo(19 * s, 7 * s);
                    break;
                case "plus":
                    p.moveTo(12 * s, 5 * s); p.lineTo(12 * s, 19 * s);
                    p.moveTo(5 * s, 12 * s); p.lineTo(19 * s, 12 * s);
                    break;
                case "close":
                    p.moveTo(6 * s, 6 * s); p.lineTo(18 * s, 18 * s);
                    p.moveTo(18 * s, 6 * s); p.lineTo(6 * s, 18 * s);
                    break;
                case "search":
                    c.drawCircle(11 * s, 11 * s, 7 * s, paint);
                    p.moveTo(20 * s, 20 * s); p.lineTo(15.8f * s, 15.8f * s);
                    break;
                case "home":
                    p.moveTo(3 * s, 11 * s); p.lineTo(12 * s, 4 * s); p.lineTo(21 * s, 11 * s);
                    p.moveTo(5 * s, 10 * s); p.lineTo(5 * s, 20 * s); p.lineTo(19 * s, 20 * s); p.lineTo(19 * s, 10 * s);
                    break;
                case "user":
                    c.drawCircle(12 * s, 8 * s, 4 * s, paint);
                    p.moveTo(4 * s, 20 * s); p.quadTo(12 * s, 14 * s, 20 * s, 20 * s);
                    break;
                case "settings":
                    c.drawCircle(12 * s, 12 * s, 3 * s, paint);
                    p.moveTo(12 * s, 3 * s); p.lineTo(12 * s, 6 * s);
                    p.moveTo(12 * s, 18 * s); p.lineTo(12 * s, 21 * s);
                    p.moveTo(3 * s, 12 * s); p.lineTo(6 * s, 12 * s);
                    p.moveTo(18 * s, 12 * s); p.lineTo(21 * s, 12 * s);
                    break;
                case "back":
                    p.moveTo(15 * s, 6 * s); p.lineTo(9 * s, 12 * s); p.lineTo(15 * s, 18 * s);
                    break;
                case "next":
                    p.moveTo(9 * s, 6 * s); p.lineTo(15 * s, 12 * s); p.lineTo(9 * s, 18 * s);
                    break;
                case "edit":
                    p.moveTo(4 * s, 20 * s); p.lineTo(8 * s, 20 * s); p.lineTo(19 * s, 9 * s);
                    p.lineTo(15 * s, 5 * s); p.lineTo(4 * s, 16 * s); p.close();
                    break;
                case "trash":
                    p.moveTo(4 * s, 7 * s); p.lineTo(20 * s, 7 * s);
                    p.moveTo(9 * s, 7 * s); p.lineTo(9 * s, 4 * s); p.lineTo(15 * s, 4 * s); p.lineTo(15 * s, 7 * s);
                    p.moveTo(6 * s, 7 * s); p.lineTo(7 * s, 20 * s); p.lineTo(17 * s, 20 * s); p.lineTo(18 * s, 7 * s);
                    break;
                case "calendar":
                    p.addRect(4 * s, 6 * s, 20 * s, 20 * s, Path.Direction.CW);
                    p.moveTo(8 * s, 3 * s); p.lineTo(8 * s, 7 * s);
                    p.moveTo(16 * s, 3 * s); p.lineTo(16 * s, 7 * s);
                    p.moveTo(4 * s, 10 * s); p.lineTo(20 * s, 10 * s);
                    break;
                case "clock":
                    c.drawCircle(12 * s, 12 * s, 9 * s, paint);
                    p.moveTo(12 * s, 7 * s); p.lineTo(12 * s, 12 * s); p.lineTo(15 * s, 14 * s);
                    break;
                case "bell":
                    p.moveTo(18 * s, 16 * s); p.lineTo(6 * s, 16 * s); p.lineTo(7 * s, 14 * s);
                    p.lineTo(7 * s, 10 * s); p.quadTo(12 * s, 3 * s, 17 * s, 10 * s); p.lineTo(17 * s, 14 * s); p.close();
                    p.moveTo(10 * s, 19 * s); p.quadTo(12 * s, 21 * s, 14 * s, 19 * s);
                    break;
                case "mail":
                    p.addRect(4 * s, 6 * s, 20 * s, 18 * s, Path.Direction.CW);
                    p.moveTo(4 * s, 7 * s); p.lineTo(12 * s, 13 * s); p.lineTo(20 * s, 7 * s);
                    break;
                case "star":
                    p.moveTo(12 * s, 3 * s); p.lineTo(14.9f * s, 8.9f * s); p.lineTo(21.4f * s, 9.8f * s);
                    p.lineTo(16.7f * s, 14.4f * s); p.lineTo(17.8f * s, 20.9f * s); p.lineTo(12 * s, 17.8f * s);
                    p.lineTo(6.2f * s, 20.9f * s); p.lineTo(7.3f * s, 14.4f * s); p.lineTo(2.6f * s, 9.8f * s);
                    p.lineTo(9.1f * s, 8.9f * s); p.close();
                    break;
                case "heart":
                    p.moveTo(12 * s, 20 * s); p.cubicTo(5 * s, 15.5f * s, 3.5f * s, 11 * s, 6 * s, 7.5f * s);
                    p.quadTo(9 * s, 4.5f * s, 12 * s, 7.5f * s); p.quadTo(15 * s, 4.5f * s, 18 * s, 7.5f * s);
                    p.cubicTo(20.5f * s, 11 * s, 19 * s, 15.5f * s, 12 * s, 20 * s);
                    break;
                case "info":
                    c.drawCircle(12 * s, 12 * s, 9 * s, paint);
                    p.moveTo(12 * s, 11 * s); p.lineTo(12 * s, 16 * s);
                    c.drawPoint(12 * s, 8 * s, paint);
                    break;
                case "warning":
                    p.moveTo(12 * s, 4 * s); p.lineTo(3 * s, 20 * s); p.lineTo(21 * s, 20 * s); p.close();
                    p.moveTo(12 * s, 10 * s); p.lineTo(12 * s, 14 * s);
                    c.drawPoint(12 * s, 17 * s, paint);
                    break;
                case "lock":
                    p.addRect(6 * s, 11 * s, 18 * s, 20 * s, Path.Direction.CW);
                    p.moveTo(8 * s, 11 * s); p.lineTo(8 * s, 8 * s); p.quadTo(12 * s, 4 * s, 16 * s, 8 * s); p.lineTo(16 * s, 11 * s);
                    break;
                case "menu":
                    p.moveTo(4 * s, 7 * s); p.lineTo(20 * s, 7 * s);
                    p.moveTo(4 * s, 12 * s); p.lineTo(20 * s, 12 * s);
                    p.moveTo(4 * s, 17 * s); p.lineTo(20 * s, 17 * s);
                    break;
                case "share":
                    p.moveTo(12 * s, 4 * s); p.lineTo(12 * s, 15 * s);
                    p.moveTo(8 * s, 8 * s); p.lineTo(12 * s, 4 * s); p.lineTo(16 * s, 8 * s);
                    p.moveTo(5 * s, 14 * s); p.lineTo(5 * s, 20 * s); p.lineTo(19 * s, 20 * s); p.lineTo(19 * s, 14 * s);
                    break;
                case "refresh":
                    c.drawCircle(12 * s, 12 * s, 7 * s, paint);
                    p.moveTo(19 * s, 5 * s); p.lineTo(19 * s, 10 * s); p.lineTo(14 * s, 10 * s);
                    break;
                case "download":
                    p.moveTo(12 * s, 4 * s); p.lineTo(12 * s, 15 * s);
                    p.moveTo(8 * s, 11 * s); p.lineTo(12 * s, 15 * s); p.lineTo(16 * s, 11 * s);
                    p.moveTo(5 * s, 19 * s); p.lineTo(19 * s, 19 * s);
                    break;
                default:
                    c.drawCircle(12 * s, 12 * s, 7 * s, paint);
                    break;
            }
            c.drawPath(p, paint);
        }
    }

    static final class ThemeStyle {
        final boolean dark;
        final int fg;
        final int accent;
        final int muted;

        ThemeStyle(boolean dark, int fg, int accent, int muted) {
            this.dark = dark;
            this.fg = fg;
            this.accent = accent;
            this.muted = muted;
        }
    }

    static final class Node {
        final int id;
        final String kind;
        View view;
        /** Where children go; null for leaf widgets. */
        final ViewGroup container;
        final FrameBox frameBox;
        LinearLayout tabsRow;
        NativeComponent nativeComponent;
        ViewTreeObserver.OnScrollChangedListener scrollListener;
        int lastScrollExtent = -1;
        final List<Node> children = new ArrayList<>();
        ThemeStyle parentTheme;
        ThemeStyle theme;
        Integer defaultTextColor;
        final JSONObject props = new JSONObject();
        List<String> events = new ArrayList<>();
        /** True while the renderer itself changes a widget, so listeners stay quiet. */
        boolean quiet;
        Drawable defaultBackground;
        boolean customBackground;

        Node(int id, String kind, View view, FrameBox container) {
            this(id, kind, view, container, container);
        }

        Node(int id, String kind, View view, ViewGroup container, FrameBox frameBox) {
            this.id = id;
            this.kind = kind;
            this.view = view;
            this.container = container;
            this.frameBox = frameBox;
        }
    }

    private final Context ctx;
    private final TinyJump tj;
    private final float density;
    private final Map<Integer, Node> nodes = new HashMap<>();
    final FrameBox root;
    private String locale = Locale.getDefault().toLanguageTag();

    String locale() {
        return locale;
    }

    void setLocale(String tag) {
        this.locale = (tag == null || tag.trim().isEmpty()) ? Locale.getDefault().toLanguageTag() : tag.trim();
        root.setLayoutDirection(isRtlLocale(this.locale) ? View.LAYOUT_DIRECTION_RTL : View.LAYOUT_DIRECTION_LTR);
        for (Node n : nodes.values()) {
            if ("Format".equals(n.kind) && n.view instanceof TextView) {
                ((TextView) n.view).setText(formatNumber(n.props));
            } else if ("Time".equals(n.kind) && n.view instanceof TextView) {
                ((TextView) n.view).setText(Times.show(n.props, this.locale));
            }
        }
    }

    private static boolean isRtlLocale(String tag) {
        if (tag == null) return false;
        String lang = tag.trim().toLowerCase(Locale.ROOT);
        int sep = lang.indexOf('-');
        int under = lang.indexOf('_');
        if (sep < 0 || (under >= 0 && under < sep)) sep = under;
        if (sep >= 0) lang = lang.substring(0, sep);
        return "ar".equals(lang) || "he".equals(lang) || "fa".equals(lang) || "ur".equals(lang)
                || "ps".equals(lang) || "sd".equals(lang) || "ug".equals(lang) || "yi".equals(lang)
                || "dv".equals(lang) || "ku".equals(lang);
    }

    private String formatNumber(JSONObject props) {
        if (!props.has("value") || props.isNull("value")) return "";
        double v = props.optDouble("value", Double.NaN);
        if (Double.isNaN(v) || Double.isInfinite(v)) return "";
        String rawLoc = props.isNull("locale") ? "" : props.optString("locale", "");
        String tag = rawLoc.isEmpty() ? this.locale : rawLoc;
        Locale loc = "pseudo".equalsIgnoreCase(tag) ? Locale.US : Locale.forLanguageTag(tag.replace('_', '-'));
        String kind = props.optString("kind", "number");
        try {
            NumberFormat nf;
            if ("currency".equals(kind)) {
                nf = NumberFormat.getCurrencyInstance(loc);
                String code = props.optString("currency", "USD");
                if (code != null && !code.isEmpty()) nf.setCurrency(Currency.getInstance(code));
            } else if ("percent".equals(kind)) {
                nf = NumberFormat.getPercentInstance(loc);
            } else {
                nf = NumberFormat.getNumberInstance(loc);
            }
            if (props.has("decimals") && !props.isNull("decimals")) {
                int d = Math.max(0, Math.min(20, props.optInt("decimals", 0)));
                nf.setMinimumFractionDigits(d);
                nf.setMaximumFractionDigits(d);
            }
            return nf.format(v);
        } catch (Exception e) {
            return String.valueOf(v);
        }
    }

    Renderer(Context ctx, ViewGroup host, TinyJump tj) {
        this.ctx = ctx;
        this.tj = tj;
        this.density = ctx.getResources().getDisplayMetrics().density;
        root = new FrameBox(ctx);
        host.addView(root, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        nodes.put(0, new Node(0, "Root", root, root));
    }

    private int px(double dp) {
        return (int) Math.round(dp * density);
    }

    /** True if the patch changed anything, so the screen needs a new layout. */
    boolean apply(JSONArray ops) throws JSONException {
        for (int i = 0; i < ops.length(); i++) {
            JSONObject op = ops.getJSONObject(i);
            int id = op.getInt("id");
            switch (op.getString("op")) {
                case "create": {
                    JSONObject props = op.getJSONObject("props");
                    Node n = create(id, op.getString("kind"), props);
                    n.events = strings(op.getJSONArray("events"));
                    nodes.put(id, n);
                    setProps(n, props, true);
                    break;
                }
                case "update": {
                    Node n = nodes.get(id);
                    if (n == null) break;
                    if (op.has("events")) n.events = strings(op.getJSONArray("events"));
                    setProps(n, op.getJSONObject("props"), false);
                    break;
                }
                case "children": {
                    Node p = nodes.get(id);
                    if (p == null || p.container == null) break;
                    JSONArray ids = op.getJSONArray("ids");
                    if ("List".equals(p.kind) && ids.length() < p.children.size()) p.lastScrollExtent = -1;
                    p.children.clear();
                    for (int j = 0; j < ids.length(); j++) {
                        Node c = nodes.get(ids.getInt(j));
                        if (c == null) continue;
                        p.children.add(c);
                        if (p.container.getChildAt(j) != c.view) {
                            if (c.view.getParent() instanceof ViewGroup) ((ViewGroup) c.view.getParent()).removeView(c.view);
                            p.container.addView(c.view, j, c.view.getLayoutParams() instanceof Frame ? c.view.getLayoutParams() : new Frame());
                        }
                        if (p.theme != null || c.parentTheme != null) propagateTheme(c, p.theme);
                    }
                    while (p.container.getChildCount() > ids.length()) p.container.removeViewAt(p.container.getChildCount() - 1);
                    break;
                }
                case "remove": {
                    Node n = nodes.remove(id);
                    if (n != null) {
                        if (n.scrollListener != null && n.view.getViewTreeObserver().isAlive()) {
                            n.view.getViewTreeObserver().removeOnScrollChangedListener(n.scrollListener);
                        }
                        if (n.nativeComponent != null) n.nativeComponent.destroy(n.view);
                        if (n.view.getParent() instanceof ViewGroup) ((ViewGroup) n.view.getParent()).removeView(n.view);
                    }
                    break;
                }
                default:
                    break;
            }
        }
        return ops.length() > 0;
    }

    /** Cleans up mounted custom native views and listeners when {@link TinyJump#stop()} is called. */
    void destroy() {
        for (Node n : nodes.values()) {
            if (n.scrollListener != null && n.view.getViewTreeObserver().isAlive()) {
                n.view.getViewTreeObserver().removeOnScrollChangedListener(n.scrollListener);
            }
            if (n.nativeComponent != null) n.nativeComponent.destroy(n.view);
        }
        nodes.clear();
    }

    /** Places nodes at the frames the core computed (dp, relative to their parent). */
    void frames(JSONArray frames) throws JSONException {
        for (int i = 0; i < frames.length(); i++) {
            JSONObject f = frames.getJSONObject(i);
            Node n = nodes.get(f.getInt("id"));
            if (n == null) continue;
            Frame lp = n.view.getLayoutParams() instanceof Frame ? (Frame) n.view.getLayoutParams() : new Frame();
            lp.x = px(f.getDouble("x"));
            lp.y = px(f.getDouble("y"));
            lp.width = px(f.getDouble("w"));
            lp.height = px(f.getDouble("h"));
            n.view.setLayoutParams(lp);
            if (f.has("content") && n.frameBox != null) {
                int extent = px(f.getDouble("content"));
                if (n.view instanceof HorizontalScrollView) {
                    n.frameBox.contentWidth = extent;
                } else {
                    n.frameBox.contentHeight = extent;
                }
                n.frameBox.requestLayout();
            }
        }
    }

    /** The natural size of a leaf in dp, wrapping at maxWidth (dp; negative = unbounded), packed as two float bit patterns. */
    long measure(int id, float maxWidth) {
        Node n = nodes.get(id);
        float w = 0;
        float h = 0;
        if (n != null) {
            int widthSpec = maxWidth < 0
                    ? View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED)
                    : View.MeasureSpec.makeMeasureSpec(px(maxWidth), View.MeasureSpec.AT_MOST);
            n.view.measure(widthSpec, View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED));
            // Round up, with 1dp of slack: layout snaps to whole dp, and text a fraction too
            // narrow would wrap its last word onto a line nobody sees.
            w = (float) Math.ceil(n.view.getMeasuredWidth() / density) + 1;
            h = (float) Math.ceil(n.view.getMeasuredHeight() / density);
        }
        return ((long) Float.floatToRawIntBits(w) << 32) | (Float.floatToRawIntBits(h) & 0xffffffffL);
    }

    private static List<String> strings(JSONArray a) throws JSONException {
        List<String> out = new ArrayList<>();
        for (int i = 0; i < a.length(); i++) out.add(a.getString(i));
        return out;
    }

    // ---------- widgets ----------

    private Node create(final int id, String kind, JSONObject props) {
        switch (kind) {
            case "Column":
            case "Row":
            case "Theme": {
                FrameBox box = new FrameBox(ctx);
                return new Node(id, kind, box, box);
            }
            case "Scroll": {
                ScrollView scroll = new ScrollView(ctx);
                FrameBox content = new FrameBox(ctx);
                scroll.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                return new Node(id, kind, scroll, content);
            }
            case "List": {
                final boolean horiz = props.optBoolean("horizontal", false);
                final FrameBox content = new FrameBox(ctx);
                final View scroll = makeListScroll(horiz, content);
                final Node n = new Node(id, kind, scroll, content);
                attachListScrollListener(n, horiz);
                return n;
            }
            case "Tabs": {
                HorizontalScrollView hsv = new HorizontalScrollView(ctx);
                hsv.setHorizontalScrollBarEnabled(false);
                LinearLayout row = new LinearLayout(ctx);
                row.setOrientation(LinearLayout.HORIZONTAL);
                hsv.addView(row, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                Node n = new Node(id, kind, hsv, null);
                n.tabsRow = row;
                return n;
            }
            case "Icon": {
                return new Node(id, kind, new IconView(ctx, density), null);
            }
            case "Button": {
                Button b = new Button(ctx);
                b.setAllCaps(false);
                final Node n = new Node(id, kind, b, null);
                b.setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        fire(n, "onPress", JSONObject.NULL);
                    }
                });
                return n;
            }
            case "Input": {
                final EditText e = new EditText(ctx);
                e.setTextAlignment(View.TEXT_ALIGNMENT_VIEW_START);
                final boolean multiline = props.optBoolean("multiline", false);
                e.setInputType(InputType.TYPE_CLASS_TEXT | (multiline ? InputType.TYPE_TEXT_FLAG_MULTI_LINE : 0));
                if (!multiline) {
                    e.setSingleLine(true);
                    e.setImeOptions(EditorInfo.IME_ACTION_DONE);
                }
                final Node n = new Node(id, kind, e, null);
                e.addTextChangedListener(new TextWatcher() {
                    @Override
                    public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

                    @Override
                    public void onTextChanged(CharSequence s, int start, int before, int count) {}

                    @Override
                    public void afterTextChanged(Editable s) {
                        String type = n.props.optString("type", "");
                        fire(n, "onChange", Times.typed(type) ? Times.value(type, n.props.optString("zone", ""), s.toString()) : s.toString());
                    }
                });
                e.setOnEditorActionListener(new TextView.OnEditorActionListener() {
                    @Override
                    public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                        boolean enter = event != null && event.getKeyCode() == KeyEvent.KEYCODE_ENTER && event.getAction() == KeyEvent.ACTION_DOWN;
                        if (action == EditorInfo.IME_ACTION_DONE || enter) {
                            String type = n.props.optString("type", "");
                            fire(n, "onSubmit", Times.typed(type) ? Times.value(type, n.props.optString("zone", ""), e.getText().toString()) : e.getText().toString());
                            return n.events.contains("onSubmit");
                        }
                        return false;
                    }
                });
                return n;
            }
            case "Toggle": {
                CheckBox c = new CheckBox(ctx);
                final Node n = new Node(id, kind, c, null);
                c.setOnCheckedChangeListener(new CompoundButton.OnCheckedChangeListener() {
                    @Override
                    public void onCheckedChanged(CompoundButton b, boolean checked) {
                        fire(n, "onChange", checked);
                    }
                });
                return n;
            }
            case "Image": {
                ImageView img = new ImageView(ctx);
                img.setAdjustViewBounds(true);
                return new Node(id, kind, img, null);
            }
            case "Time":
            case "Format":
            case "Text": {
                TextView t = new TextView(ctx);
                // Align to the layout's start (right in RTL), like the web, not to the text's own direction.
                t.setTextAlignment(View.TEXT_ALIGNMENT_VIEW_START);
                return new Node(id, kind, t, null);
            }
            case "Spacer":
                return new Node(id, kind, new View(ctx), null);
            default: {
                NativeComponent comp = TinyJump.nativeComponent(kind);
                if (comp != null) {
                    final Node[] holder = new Node[1];
                    View v = comp.create(ctx, copyJson(props), new NativeComponent.EventSink() {
                        @Override
                        public void emit(String event, Object value) {
                            if (holder[0] != null) fire(holder[0], event, value == null ? JSONObject.NULL : value);
                        }
                    });
                    ViewGroup hostBox = comp.container(v);
                    FrameBox inner = null;
                    if (hostBox != null) {
                        // Wrap the host's ViewGroup in a FrameBox so Taffy Frame LayoutParams work safely.
                        inner = new FrameBox(ctx);
                        hostBox.addView(inner, new ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
                    }
                    Node n = new Node(id, kind, v, inner);
                    n.nativeComponent = comp;
                    holder[0] = n;
                    return n;
                }
                TextView missing = new TextView(ctx);
                missing.setText("<" + kind + ">");
                return new Node(id, kind, missing, null);
            }
        }
    }

    private View makeListScroll(boolean horiz, FrameBox content) {
        if (horiz) {
            HorizontalScrollView hsv = new HorizontalScrollView(ctx);
            hsv.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.MATCH_PARENT));
            return hsv;
        }
        ScrollView sv = new ScrollView(ctx);
        sv.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        return sv;
    }

    private void attachListScrollListener(final Node n, final boolean horiz) {
        final View scroll = n.view;
        final FrameBox content = n.frameBox;
        final int threshold = px(120);
        n.scrollListener = new ViewTreeObserver.OnScrollChangedListener() {
            @Override
            public void onScrollChanged() {
                if (!n.events.contains("onEndReached") || content == null) return;
                int extent = horiz ? content.getWidth() : content.getHeight();
                int viewSize = horiz ? scroll.getWidth() : scroll.getHeight();
                int offset = horiz ? scroll.getScrollX() : scroll.getScrollY();
                if (extent < n.lastScrollExtent) n.lastScrollExtent = -1;
                if (extent > viewSize && extent - offset - viewSize <= threshold && n.lastScrollExtent != extent) {
                    n.lastScrollExtent = extent;
                    fire(n, "onEndReached", JSONObject.NULL);
                }
            }
        };
        scroll.getViewTreeObserver().addOnScrollChangedListener(n.scrollListener);
    }

    private void swapListAxis(Node n, boolean horiz) {
        if (n.frameBox == null) return;
        boolean wasHoriz = n.view instanceof HorizontalScrollView;
        if (wasHoriz == horiz) return;
        View old = n.view;
        if (n.scrollListener != null && old.getViewTreeObserver().isAlive()) {
            old.getViewTreeObserver().removeOnScrollChangedListener(n.scrollListener);
        }
        if (n.frameBox.getParent() instanceof ViewGroup) ((ViewGroup) n.frameBox.getParent()).removeView(n.frameBox);
        View next = makeListScroll(horiz, n.frameBox);
        next.setTag(old.getTag());
        next.setContentDescription(old.getContentDescription());
        if (old.getParent() instanceof ViewGroup) {
            ViewGroup parent = (ViewGroup) old.getParent();
            int idx = parent.indexOfChild(old);
            ViewGroup.LayoutParams lp = old.getLayoutParams();
            parent.removeViewAt(idx);
            parent.addView(next, idx, lp != null ? lp : new Frame());
        }
        n.view = next;
        n.lastScrollExtent = -1;
        attachListScrollListener(n, horiz);
        if (n.customBackground) restyle(n);
    }

    private static JSONObject copyJson(JSONObject src) {
        JSONObject out = new JSONObject();
        for (Iterator<String> it = src.keys(); it.hasNext(); ) {
            String k = it.next();
            try { out.put(k, src.get(k)); } catch (JSONException ignored) {}
        }
        return out;
    }

    private void rebuildTabs(final Node n) {
        LinearLayout row = n.tabsRow;
        if (row == null) return;
        row.removeAllViews();
        JSONArray items = n.props.optJSONArray("items");
        if (items == null) return;
        String current = n.props.isNull("value") ? null : n.props.optString("value", null);
        int minH = px(44);
        int padH = px(14);
        int activeColor = n.theme != null ? n.theme.accent : 0xFF2563EB;
        int inactiveColor = n.theme != null ? n.theme.muted : 0xFF374151;
        for (int i = 0; i < items.length(); i++) {
            Object raw = items.opt(i);
            final String val;
            final String label;
            if (raw instanceof JSONObject) {
                JSONObject o = (JSONObject) raw;
                String rv = o.isNull("value") ? null : o.optString("value", null);
                String rl = o.isNull("label") ? null : o.optString("label", null);
                val = rv != null ? rv : (rl != null ? rl : "");
                label = rl != null ? rl : val;
            } else if (raw == null || raw == JSONObject.NULL) {
                val = "";
                label = "";
            } else {
                val = String.valueOf(raw);
                label = val;
            }
            boolean active = val.equals(current);
            Button b = new Button(ctx);
            b.setAllCaps(false);
            b.setText(label);
            b.setTag(val);
            b.setMinimumHeight(minH);
            b.setMinimumWidth(minH);
            b.setPadding(padH, 0, padH, 0);
            b.setSelected(active);
            b.setTypeface(null, active ? Typeface.BOLD : Typeface.NORMAL);
            b.setBackgroundColor(Color.TRANSPARENT);
            b.setTextColor(active ? activeColor : inactiveColor);
            b.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    fire(n, "onChange", val);
                }
            });
            row.addView(b, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        }
    }

    private void updateTabSelection(Node n) {
        LinearLayout row = n.tabsRow;
        if (row == null) return;
        String current = n.props.isNull("value") ? null : n.props.optString("value", null);
        int activeColor = n.theme != null ? n.theme.accent : 0xFF2563EB;
        int inactiveColor = n.theme != null ? n.theme.muted : 0xFF374151;
        for (int i = 0; i < row.getChildCount(); i++) {
            View child = row.getChildAt(i);
            if (!(child instanceof Button)) continue;
            Button b = (Button) child;
            boolean active = current != null && current.equals(b.getTag());
            b.setSelected(active);
            b.setTypeface(null, active ? Typeface.BOLD : Typeface.NORMAL);
            b.setTextColor(active ? activeColor : inactiveColor);
        }
    }

    private ThemeStyle resolveTheme(Node n) {
        boolean hasAny = n.props.has("mode") || n.props.has("accent") || n.props.has("color") || n.props.has("surface") || n.props.has("muted");
        if (!hasAny) return null;
        String mode = n.props.optString("mode", "");
        boolean dark = "dark".equals(mode)
                || ("system".equals(mode) && (ctx.getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES);
        Integer customFg = n.props.has("color") ? color(n.props.optString("color", "")) : null;
        Integer customAccent = n.props.has("accent") ? color(n.props.optString("accent", "")) : null;
        Integer customMuted = n.props.has("muted") ? color(n.props.optString("muted", "")) : null;
        int fg = customFg != null ? customFg : (dark ? 0xFFF9FAFB : 0xFF111827);
        int accent = customAccent != null ? customAccent : (dark ? 0xFF60A5FA : 0xFF2563EB);
        int muted = customMuted != null ? customMuted : (dark ? 0xFF9CA3AF : 0xFF6B7280);
        return new ThemeStyle(dark, fg, accent, muted);
    }

    private void propagateTheme(Node n, ThemeStyle inherited) {
        n.parentTheme = inherited;
        ThemeStyle active = "Theme".equals(n.kind) ? resolveTheme(n) : null;
        ThemeStyle effective = active != null ? active : inherited;
        n.theme = effective;
        applyThemeToNode(n, effective);
        for (Node c : n.children) propagateTheme(c, effective);
    }

    private void applyThemeToNode(Node n, ThemeStyle t) {
        if ("Tabs".equals(n.kind)) {
            updateTabSelection(n);
            return;
        }
        if ("Icon".equals(n.kind) && n.view instanceof IconView && !n.props.has("color")) {
            ((IconView) n.view).setIconColor(t != null ? t.fg : 0xFF111827);
            return;
        }
        if (("Text".equals(n.kind) || "Time".equals(n.kind) || "Format".equals(n.kind)) && n.view instanceof TextView && !n.props.has("color")) {
            TextView tv = (TextView) n.view;
            if (n.defaultTextColor == null) n.defaultTextColor = tv.getCurrentTextColor();
            tv.setTextColor(t != null ? t.fg : n.defaultTextColor);
        }
    }

    private void fire(Node n, String event, Object value) {
        if (n.quiet || !n.events.contains(event)) return;
        try {
            tj.send(TinyJump.input("ui").put("target", new JSONObject().put("node", n.id)).put("event", event).put("value", value));
        } catch (JSONException e) {
            throw new IllegalStateException(e);
        }
    }

    private void setProps(Node n, JSONObject props, boolean isCreate) throws JSONException {
        boolean box = false;
        boolean tabsItemsChanged = false;
        boolean tabsValueChanged = false;
        boolean themeChanged = false;
        boolean listAxisChanged = false;
        for (Iterator<String> it = props.keys(); it.hasNext(); ) {
            String k = it.next();
            Object v = props.get(k);
            boolean none = v == JSONObject.NULL;
            if (none) n.props.remove(k); else n.props.put(k, v);
            if ("background".equals(k) || "radius".equals(k) || "border".equals(k)) box = true;
            if ("Tabs".equals(n.kind) && "items".equals(k)) tabsItemsChanged = true;
            if ("Tabs".equals(n.kind) && "value".equals(k)) tabsValueChanged = true;
            if ("List".equals(n.kind) && "horizontal".equals(k)) listAxisChanged = true;
            if ("Theme".equals(n.kind) && ("mode".equals(k) || "surface".equals(k) || "accent".equals(k) || "muted".equals(k) || "color".equals(k) || "background".equals(k))) {
                themeChanged = true;
                box = true;
            }
            if (n.nativeComponent != null) {
                if ("id".equals(k)) n.view.setTag(none ? null : v);
                else if ("label".equals(k)) n.view.setContentDescription(none ? null : String.valueOf(v));
            } else {
                setProp(n, k, v, none);
            }
        }
        if (n.nativeComponent != null && !isCreate) {
            n.quiet = true;
            try {
                n.nativeComponent.update(n.view, copyJson(n.props), copyJson(props));
            } finally {
                n.quiet = false;
            }
        }
        if (listAxisChanged && !isCreate) swapListAxis(n, n.props.optBoolean("horizontal", false));
        if (tabsItemsChanged) rebuildTabs(n);
        else if (tabsValueChanged) updateTabSelection(n);
        if (box) restyle(n);
        if (themeChanged) propagateTheme(n, n.parentTheme);
    }

    /** Visual props. Layout props (width, gap, align, grow, ...) are the core's job. */
    private void setProp(Node n, String k, Object v, boolean none) {
        View view = n.view;
        TextView text = view instanceof TextView ? (TextView) view : null;
        n.quiet = true;
        try {
            if ("Icon".equals(n.kind) && view instanceof IconView) {
                IconView icon = (IconView) view;
                if ("name".equals(k)) { icon.setIconName(none ? "" : String.valueOf(v)); return; }
                if ("size".equals(k)) { icon.setIconSize(!none && v instanceof Number ? ((Number) v).floatValue() : 20f); return; }
                if ("color".equals(k)) {
                    Integer c = none ? null : color(String.valueOf(v));
                    icon.setIconColor(c != null ? c : (n.theme != null ? n.theme.fg : 0xFF111827));
                    return;
                }
            }
            if ("Time".equals(n.kind) && ("value".equals(k) || "show".equals(k) || "style".equals(k) || "zone".equals(k))) {
                if (text != null) text.setText(Times.show(n.props, this.locale));
                return;
            }
            if ("Format".equals(n.kind) && ("value".equals(k) || "kind".equals(k) || "currency".equals(k) || "decimals".equals(k) || "locale".equals(k))) {
                if (text != null) text.setText(formatNumber(n.props));
                return;
            }
            String type = n.props.optString("type", "");
            if (view instanceof EditText && ("type".equals(k) || "zone".equals(k) || ("value".equals(k) && Times.typed(type)))) {
                typed((EditText) view, n, type);
                // Shown again whichever of type, zone or value changed.
                Object value = n.props.opt("value");
                String s = Times.typed(type) ? Times.text(type, n.props.optString("zone", ""), value) : value == null ? "" : String.valueOf(value);
                if (!s.contentEquals(((EditText) view).getText())) ((EditText) view).setText(s);
                return;
            }
            switch (k) {
                case "text":
                    if (text != null) text.setText(none ? "" : String.valueOf(v));
                    break;
                case "value":
                    if (view instanceof CheckBox) {
                        ((CheckBox) view).setChecked(!none && Boolean.TRUE.equals(v));
                    } else if (view instanceof EditText) {
                        String s = none ? "" : String.valueOf(v);
                        EditText e = (EditText) view;
                        if (!s.contentEquals(e.getText())) {
                            e.setText(s);
                            e.setSelection(s.length());
                        }
                    }
                    break;
                case "placeholder":
                    if (text != null) text.setHint(none ? "" : String.valueOf(v));
                    break;
                case "disabled":
                    view.setEnabled(none || !Boolean.TRUE.equals(v));
                    break;
                case "secure":
                    if (view instanceof EditText) {
                        boolean secure = !none && Boolean.TRUE.equals(v);
                        ((EditText) view).setInputType(InputType.TYPE_CLASS_TEXT | (secure ? InputType.TYPE_TEXT_VARIATION_PASSWORD : 0));
                    }
                    break;
                case "lines":
                    if (text != null) {
                        text.setMaxLines(none ? Integer.MAX_VALUE : ((Number) v).intValue());
                        text.setEllipsize(none ? null : TextUtils.TruncateAt.END);
                    }
                    break;
                case "src":
                    if (view instanceof ImageView) loadImage((ImageView) view, none ? null : String.valueOf(v));
                    break;
                case "fit":
                    if (view instanceof ImageView) ((ImageView) view).setScaleType(scale(none ? "" : String.valueOf(v)));
                    break;
                case "id":
                    view.setTag(none ? null : v);
                    break;
                case "label":
                    view.setContentDescription(none ? null : String.valueOf(v));
                    break;
                case "padding":
                    // Containers are padded by the core's layout; leaves draw their own padding.
                    if (n.container == null) {
                        int p = none ? 0 : px(((Number) v).doubleValue());
                        view.setPadding(p, p, p, p);
                    }
                    break;
                case "color":
                    if (text != null) {
                        if (n.defaultTextColor == null) n.defaultTextColor = text.getCurrentTextColor();
                        Integer c = none ? null : color(String.valueOf(v));
                        if (c != null) text.setTextColor(c);
                        else if (none) text.setTextColor(n.theme != null ? n.theme.fg : n.defaultTextColor);
                    }
                    break;
                case "size":
                    if (text != null && !none) text.setTextSize(TypedValue.COMPLEX_UNIT_SP, ((Number) v).floatValue());
                    break;
                case "weight":
                    if (text != null) text.setTypeface(null, !none && isBold(v) ? Typeface.BOLD : Typeface.NORMAL);
                    break;
                default:
                    break;
            }
        } finally {
            n.quiet = false;
        }
    }

    private static boolean isBold(Object v) {
        if (v instanceof Number) return ((Number) v).intValue() >= 600;
        String s = String.valueOf(v);
        return "bold".equals(s) || "bolder".equals(s);
    }

    private static ImageView.ScaleType scale(String fit) {
        switch (fit) {
            case "cover":
                return ImageView.ScaleType.CENTER_CROP;
            case "fill":
                return ImageView.ScaleType.FIT_XY;
            default:
                return ImageView.ScaleType.FIT_CENTER;
        }
    }

    private void loadImage(final ImageView view, final String src) {
        view.setImageDrawable(null);
        if (src == null || !(src.startsWith("https://") || src.startsWith("http://"))) return;
        tj.io.execute(new Runnable() {
            @Override
            public void run() {
                try (InputStream in = new URL(src).openStream()) {
                    final Bitmap bmp = BitmapFactory.decodeStream(in);
                    tj.main.post(new Runnable() {
                        @Override
                        public void run() {
                            view.setImageBitmap(bmp);
                        }
                    });
                } catch (Exception e) {
                    Log.w(TinyJump.TAG, "image " + src + ": " + e);
                }
            }
        });
    }

    /** Background color, corner radius and border share one drawable. */
    private void restyle(Node n) {
        Integer bg = n.props.has("background") ? color(n.props.optString("background")) : null;
        if (bg == null && "Theme".equals(n.kind)) {
            String surface = n.props.has("surface") ? n.props.optString("surface", "") : "";
            bg = !surface.isEmpty() ? color(surface) : null;
            if (bg == null && n.props.has("mode")) {
                String mode = n.props.optString("mode", "");
                boolean dark = "dark".equals(mode)
                        || ("system".equals(mode) && (ctx.getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES);
                bg = dark ? 0xFF111827 : 0xFFFFFFFF;
            }
        }
        double radius = n.props.optDouble("radius", 0);
        Object border = n.props.opt("border");
        if (bg == null && radius == 0 && border == null) {
            if (n.customBackground) n.view.setBackground(n.defaultBackground);
            n.customBackground = false;
            return;
        }
        if (!n.customBackground) {
            n.defaultBackground = n.view.getBackground();
            n.customBackground = true;
        }
        GradientDrawable d = new GradientDrawable();
        d.setColor(bg != null ? bg : Color.TRANSPARENT);
        d.setCornerRadius(px(radius));
        if (border instanceof Number) {
            d.setStroke(px(((Number) border).doubleValue()), Color.BLACK);
        } else if (border != null) {
            int width = 1;
            int c = Color.BLACK;
            for (String part : String.valueOf(border).split("\\s+")) {
                if (part.endsWith("px")) width = Integer.parseInt(part.substring(0, part.length() - 2));
                Integer parsed = color(part);
                if (parsed != null) c = parsed;
            }
            d.setStroke(px(width), c);
        }
        n.view.setBackground(d);
    }

    /** CSS-style colors: #rgb, #rrggbb, #rrggbbaa, or a basic name. */
    static Integer color(String s) {
        if (s == null) return null;
        s = s.trim();
        try {
            if (s.startsWith("#")) {
                String h = s.substring(1);
                if (h.length() == 3) h = "" + h.charAt(0) + h.charAt(0) + h.charAt(1) + h.charAt(1) + h.charAt(2) + h.charAt(2);
                if (h.length() == 6) return (int) (0xFF000000L | Long.parseLong(h, 16));
                if (h.length() == 8) {
                    long rgba = Long.parseLong(h, 16);
                    return (int) (((rgba & 0xFF) << 24) | (rgba >>> 8));
                }
                return null;
            }
            if ("transparent".equals(s)) return Color.TRANSPARENT;
            return Color.parseColor(s);
        } catch (IllegalArgumentException e) {
            return null;
        }
    }

    // ---------- automation ----------

    /** The window size in dp, for overflow checks. */
    JSONObject viewport() throws JSONException {
        View window = root.getRootView();
        return new JSONObject().put("w", Math.round(window.getWidth() / density)).put("h", Math.round(window.getHeight() / density));
    }

    /** The window as PNG, for visual tests; null before the first layout. */
    byte[] screenshot() {
        View window = root.getRootView();
        if (window.getWidth() == 0 || window.getHeight() == 0) return null;
        Bitmap bitmap = Bitmap.createBitmap(window.getWidth(), window.getHeight(), Bitmap.Config.ARGB_8888);
        window.draw(new Canvas(bitmap));
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, out);
        bitmap.recycle();
        return out.toByteArray();
    }

    /** Sets an input up for its `type`: a number keypad, or the system's pickers for times. */
    private void typed(final EditText e, final Node n, String type) {
        boolean picker = "date".equals(type) || "time".equals(type) || "datetime".equals(type);
        e.setInputType("number".equals(type) ? InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL | InputType.TYPE_NUMBER_FLAG_SIGNED : InputType.TYPE_CLASS_TEXT);
        e.setFocusable(!picker);
        e.setOnClickListener(!picker ? null : new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                final String t = n.props.optString("type", "");
                Times.pick(e.getContext(), t, n.props.optString("zone", ""), n.props.opt("value"), new Times.Picked() {
                    @Override
                    public void value(Object ms) {
                        fire(n, "onChange", ms);
                    }
                });
            }
        });
    }

    /**
     * An agent typing into a date, time or number input: its text becomes the value, as a
     * person's entry does (`POST /_tj/input`).
     */
    void forAgent(JSONObject input) throws JSONException {
        if (!"ui".equals(input.optString("type")) || !(input.opt("value") instanceof String)) return;
        JSONObject target = input.optJSONObject("target");
        if (target == null) return;
        Node node = null;
        if (target.has("node")) node = nodes.get(target.getInt("node"));
        // By id, or by the text an agent sees on it: its label or placeholder.
        String text = target.optString("text", "\u0000");
        for (Node c : nodes.values()) {
            if (node != null) break;
            if (target.optString("id", "\u0000").equals(c.props.optString("id")) || text.equals(c.props.optString("label")) || text.equals(c.props.optString("placeholder"))) node = c;
        }
        if (node == null || !"Input".equals(node.kind) || !Times.typed(node.props.optString("type", ""))) return;
        input.put("value", Times.value(node.props.optString("type"), node.props.optString("zone", ""), input.getString("value")));
    }

    /** Adds bounds (dp, on screen) and visibility to an inspect tree, like the web host does. */
    void addBounds(JSONArray tree) throws JSONException {
        for (int i = 0; i < tree.length(); i++) {
            JSONObject n = tree.getJSONObject(i);
            Node node = nodes.get(n.getInt("id"));
            if (node != null && ("Time".equals(node.kind) || "Format".equals(node.kind))) {
                // What the platform made of it, so agents read what people see.
                n.getJSONObject("props").put("text", ((TextView) node.view).getText().toString());
            }
            if (node != null) {
                int[] loc = new int[2];
                node.view.getLocationOnScreen(loc);
                Rect visible = new Rect();
                boolean shown = node.view.isShown() && node.view.getGlobalVisibleRect(visible);
                n.put("bounds", new JSONObject()
                        .put("x", Math.round(loc[0] / density))
                        .put("y", Math.round(loc[1] / density))
                        .put("w", Math.round(node.view.getWidth() / density))
                        .put("h", Math.round(node.view.getHeight() / density)));
                n.put("visible", shown);
            }
            addBounds(n.getJSONArray("children"));
        }
    }

    /** The root's size in dp, or null before it has one. */
    float[] viewportDp() {
        if (root.getWidth() == 0 || root.getHeight() == 0) return null;
        return new float[] {root.getWidth() / density, root.getHeight() / density};
    }
}
