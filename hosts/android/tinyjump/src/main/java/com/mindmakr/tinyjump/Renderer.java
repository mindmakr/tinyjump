package com.mindmakr.tinyjump;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Canvas;
import android.graphics.Color;
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
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.CompoundButton;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.net.URL;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
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
        /** Content height in pixels when this box scrolls; 0 otherwise. */
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
            int h = MeasureSpec.getMode(heightSpec) == MeasureSpec.EXACTLY ? MeasureSpec.getSize(heightSpec) : contentHeight;
            setMeasuredDimension(MeasureSpec.getSize(widthSpec), h);
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

    static final class Node {
        final int id;
        final String kind;
        final View view;
        /** Where children go; null for leaf widgets. */
        final FrameBox container;
        final JSONObject props = new JSONObject();
        List<String> events = new ArrayList<>();
        /** True while the renderer itself changes a widget, so listeners stay quiet. */
        boolean quiet;
        Drawable defaultBackground;
        boolean customBackground;

        Node(int id, String kind, View view, FrameBox container) {
            this.id = id;
            this.kind = kind;
            this.view = view;
            this.container = container;
        }
    }

    private final Context ctx;
    private final TinyJump tj;
    private final float density;
    private final Map<Integer, Node> nodes = new HashMap<>();
    final FrameBox root;

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
                    setProps(n, props);
                    break;
                }
                case "update": {
                    Node n = nodes.get(id);
                    if (n == null) break;
                    setProps(n, op.getJSONObject("props"));
                    if (op.has("events")) n.events = strings(op.getJSONArray("events"));
                    break;
                }
                case "children": {
                    Node p = nodes.get(id);
                    if (p == null || p.container == null) break;
                    JSONArray ids = op.getJSONArray("ids");
                    for (int j = 0; j < ids.length(); j++) {
                        Node c = nodes.get(ids.getInt(j));
                        if (c == null || p.container.getChildAt(j) == c.view) continue;
                        if (c.view.getParent() instanceof ViewGroup) ((ViewGroup) c.view.getParent()).removeView(c.view);
                        p.container.addView(c.view, j, c.view.getLayoutParams() instanceof Frame ? c.view.getLayoutParams() : new Frame());
                    }
                    while (p.container.getChildCount() > ids.length()) p.container.removeViewAt(p.container.getChildCount() - 1);
                    break;
                }
                case "remove": {
                    Node n = nodes.remove(id);
                    if (n != null && n.view.getParent() instanceof ViewGroup) ((ViewGroup) n.view.getParent()).removeView(n.view);
                    break;
                }
                default:
                    break;
            }
        }
        return ops.length() > 0;
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
            if (f.has("content") && n.container != null) {
                n.container.contentHeight = px(f.getDouble("content"));
                n.container.requestLayout();
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
            case "Row": {
                FrameBox box = new FrameBox(ctx);
                return new Node(id, kind, box, box);
            }
            case "Scroll": {
                ScrollView scroll = new ScrollView(ctx);
                FrameBox content = new FrameBox(ctx);
                scroll.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                return new Node(id, kind, scroll, content);
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
            case "Text": {
                TextView t = new TextView(ctx);
                // Align to the layout's start (right in RTL), like the web, not to the text's own direction.
                t.setTextAlignment(View.TEXT_ALIGNMENT_VIEW_START);
                return new Node(id, kind, t, null);
            }
            default: // Spacer
                return new Node(id, kind, new View(ctx), null);
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

    private void setProps(Node n, JSONObject props) throws JSONException {
        boolean box = false;
        for (Iterator<String> it = props.keys(); it.hasNext(); ) {
            String k = it.next();
            Object v = props.get(k);
            boolean none = v == JSONObject.NULL;
            if (none) n.props.remove(k); else n.props.put(k, v);
            if ("background".equals(k) || "radius".equals(k) || "border".equals(k)) box = true;
            setProp(n, k, v, none);
        }
        if (box) restyle(n);
    }

    /** Visual props. Layout props (width, gap, align, grow, ...) are the core's job. */
    private void setProp(Node n, String k, Object v, boolean none) {
        View view = n.view;
        TextView text = view instanceof TextView ? (TextView) view : null;
        n.quiet = true;
        try {
            if ("Time".equals(n.kind)) {
                if (text != null && ("value".equals(k) || "show".equals(k) || "style".equals(k) || "zone".equals(k))) text.setText(Times.show(n.props));
                return;
            }
            String type = n.props.optString("type", "");
            if (view instanceof EditText && ("type".equals(k) || "zone".equals(k) || ("value".equals(k) && Times.typed(type)))) {
                typed((EditText) view, n, type);
                if (!"value".equals(k)) return;
                String s = Times.text(type, n.props.optString("zone", ""), v);
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
                        Integer c = none ? null : color(String.valueOf(v));
                        if (c != null) text.setTextColor(c);
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
        for (Node c : nodes.values()) if (node == null && target.optString("id", "\u0000").equals(c.props.optString("id"))) node = c;
        if (node == null || !"Input".equals(node.kind) || !Times.typed(node.props.optString("type", ""))) return;
        input.put("value", Times.value(node.props.optString("type"), node.props.optString("zone", ""), input.getString("value")));
    }

    /** Adds bounds (dp, on screen) and visibility to an inspect tree, like the web host does. */
    void addBounds(JSONArray tree) throws JSONException {
        for (int i = 0; i < tree.length(); i++) {
            JSONObject n = tree.getJSONObject(i);
            Node node = nodes.get(n.getInt("id"));
            if (node != null && "Time".equals(node.kind)) {
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
