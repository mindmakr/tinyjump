package com.mindmakr.tinyjump;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Color;
import android.graphics.Rect;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.text.Editable;
import android.text.InputType;
import android.text.TextUtils;
import android.text.TextWatcher;
import android.util.TypedValue;
import android.view.Gravity;
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
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.InputStream;
import java.net.URL;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.IdentityHashMap;
import java.util.Iterator;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** Applies view-tree patches to native Views. Mirrors the catalog in crates/tj-core/src/ui/catalog.rs. */
final class Renderer {
    private static final Set<String> LAYOUT_PROPS = set("width", "height", "grow", "margin");
    private static final Set<String> ARRANGE_PROPS = set("gap", "align", "justify");
    private static final Set<String> BOX_PROPS = set("background", "radius", "border");

    static final class Node {
        final int id;
        final String kind;
        final View view;
        /** Where children go; null for leaf widgets. */
        final ViewGroup container;
        final JSONObject props = new JSONObject();
        List<String> events = new ArrayList<>();
        List<Integer> children = new ArrayList<>();
        final List<View> spacers = new ArrayList<>();
        int parent = -1;
        /** True while the renderer itself changes a widget, so listeners stay quiet. */
        boolean quiet;
        Drawable defaultBackground;
        boolean customBackground;

        Node(int id, String kind, View view, ViewGroup container) {
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
    private final Map<View, Node> byView = new IdentityHashMap<>();

    Renderer(Context ctx, ViewGroup root, TinyJump tj) {
        this.ctx = ctx;
        this.tj = tj;
        this.density = ctx.getResources().getDisplayMetrics().density;
        nodes.put(0, new Node(0, "Root", root, root));
    }

    private static Set<String> set(String... items) {
        Set<String> s = new LinkedHashSet<>();
        java.util.Collections.addAll(s, items);
        return s;
    }

    int dp(double v) {
        return (int) (v * density + 0.5);
    }

    void apply(JSONArray ops) throws JSONException {
        Set<Node> rearrange = new LinkedHashSet<>();
        for (int i = 0; i < ops.length(); i++) {
            JSONObject op = ops.getJSONObject(i);
            int id = op.getInt("id");
            switch (op.getString("op")) {
                case "create": {
                    JSONObject props = op.getJSONObject("props");
                    Node n = create(id, op.getString("kind"), props);
                    n.events = strings(op.getJSONArray("events"));
                    nodes.put(id, n);
                    byView.put(n.view, n);
                    setProps(n, props);
                    break;
                }
                case "update": {
                    Node n = nodes.get(id);
                    if (n == null) break;
                    JSONObject props = op.getJSONObject("props");
                    setProps(n, props);
                    if (op.has("events")) n.events = strings(op.getJSONArray("events"));
                    for (Iterator<String> it = props.keys(); it.hasNext(); ) {
                        String k = it.next();
                        if (LAYOUT_PROPS.contains(k) && nodes.containsKey(n.parent)) rearrange.add(nodes.get(n.parent));
                        if (ARRANGE_PROPS.contains(k)) rearrange.add(n);
                    }
                    break;
                }
                case "children": {
                    Node p = nodes.get(id);
                    if (p == null) break;
                    p.children = new ArrayList<>();
                    JSONArray ids = op.getJSONArray("ids");
                    for (int j = 0; j < ids.length(); j++) {
                        p.children.add(ids.getInt(j));
                        Node c = nodes.get(ids.getInt(j));
                        if (c != null) c.parent = id;
                    }
                    rearrange.add(p);
                    break;
                }
                case "remove": {
                    Node n = nodes.remove(id);
                    if (n == null) break;
                    byView.remove(n.view);
                    if (n.view.getParent() instanceof ViewGroup) ((ViewGroup) n.view.getParent()).removeView(n.view);
                    break;
                }
                default:
                    break;
            }
        }
        for (Node p : rearrange) arrange(p);
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
                LinearLayout l = new LinearLayout(ctx);
                l.setOrientation("Row".equals(kind) ? LinearLayout.HORIZONTAL : LinearLayout.VERTICAL);
                return new Node(id, kind, l, l);
            }
            case "Scroll": {
                ScrollView s = new ScrollView(ctx);
                LinearLayout inner = new LinearLayout(ctx);
                inner.setOrientation(LinearLayout.VERTICAL);
                s.addView(inner, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
                return new Node(id, kind, s, inner);
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
                        fire(n, "onChange", s.toString());
                    }
                });
                e.setOnEditorActionListener(new TextView.OnEditorActionListener() {
                    @Override
                    public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                        boolean enter = event != null && event.getKeyCode() == KeyEvent.KEYCODE_ENTER && event.getAction() == KeyEvent.ACTION_DOWN;
                        if (action == EditorInfo.IME_ACTION_DONE || enter) {
                            fire(n, "onSubmit", e.getText().toString());
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
            case "Text":
                return new Node(id, kind, new TextView(ctx), null);
            default: // Spacer
                return new Node(id, kind, new View(ctx), null);
        }
    }

    private void fire(Node n, String event, Object value) {
        if (n.quiet || !n.events.contains(event)) return;
        try {
            JSONObject in = TinyJump.input("ui").put("target", new JSONObject().put("node", n.id)).put("event", event).put("value", value);
            tj.send(in);
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
            if (BOX_PROPS.contains(k)) box = true;
            setProp(n, k, v, none);
        }
        if (box) restyle(n);
    }

    private void setProp(final Node n, String k, Object v, boolean none) {
        View view = n.view;
        TextView text = view instanceof TextView ? (TextView) view : null;
        n.quiet = true;
        try {
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
                case "padding": {
                    int p = none ? 0 : dp(((Number) v).doubleValue());
                    view.setPadding(p, p, p, p);
                    break;
                }
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
                    break; // layout and box props are applied in arrange() and restyle()
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
                    android.util.Log.w(TinyJump.TAG, "image " + src + ": " + e);
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
        d.setCornerRadius(dp(radius));
        if (border instanceof Number) {
            d.setStroke(dp(((Number) border).doubleValue()), Color.BLACK);
        } else if (border != null) {
            int width = 1;
            int c = Color.BLACK;
            for (String part : String.valueOf(border).split("\\s+")) {
                if (part.endsWith("px")) width = Integer.parseInt(part.substring(0, part.length() - 2));
                Integer parsed = color(part);
                if (parsed != null) c = parsed;
            }
            d.setStroke(dp(width), c);
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

    // ---------- layout ----------

    /** Puts a node's children in order (with spacers for between/around) and sets their layout. */
    private void arrange(Node p) {
        ViewGroup c = p.container;
        if (c == null) return;
        boolean vertical = !(c instanceof LinearLayout) || ((LinearLayout) c).getOrientation() == LinearLayout.VERTICAL;
        String justify = p.props.optString("justify", "start");
        String align = p.props.optString("align", vertical ? "stretch" : "center");
        boolean between = "between".equals(justify);
        boolean around = "around".equals(justify);

        List<View> want = new ArrayList<>();
        int spacer = 0;
        if (around) want.add(spacer(p, spacer++));
        for (int i = 0; i < p.children.size(); i++) {
            Node child = nodes.get(p.children.get(i));
            if (child == null) continue;
            if (i > 0 && (between || around)) want.add(spacer(p, spacer++));
            want.add(child.view);
        }
        if (around) want.add(spacer(p, spacer));

        for (int i = 0; i < want.size(); i++) {
            View v = want.get(i);
            if (c.getChildAt(i) == v) continue;
            if (v.getParent() instanceof ViewGroup) ((ViewGroup) v.getParent()).removeView(v);
            c.addView(v, i);
        }
        while (c.getChildCount() > want.size()) c.removeViewAt(c.getChildCount() - 1);

        int gap = dp(p.props.optDouble("gap", 0));
        boolean first = true;
        for (View v : want) {
            Node child = byView.get(v);
            if (child == null) {
                v.setLayoutParams(new LinearLayout.LayoutParams(0, 0, 1f)); // spacer
                continue;
            }
            v.setLayoutParams(params(c, vertical, align, child, first ? 0 : gap));
            first = false;
        }
        if (c instanceof LinearLayout) ((LinearLayout) c).setGravity(gravity(vertical, align, justify));
    }

    private View spacer(Node p, int i) {
        while (p.spacers.size() <= i) p.spacers.add(new View(ctx));
        return p.spacers.get(i);
    }

    private ViewGroup.LayoutParams params(ViewGroup parent, boolean vertical, String align, Node child, int gap) {
        JSONObject pr = child.props;
        int crossDefault = "stretch".equals(align) ? ViewGroup.LayoutParams.MATCH_PARENT : ViewGroup.LayoutParams.WRAP_CONTENT;
        int w = size(pr.opt("width"), vertical ? crossDefault : ViewGroup.LayoutParams.WRAP_CONTENT);
        int h = size(pr.opt("height"), vertical ? ViewGroup.LayoutParams.WRAP_CONTENT : crossDefault);
        int m = dp(pr.optDouble("margin", 0));
        if (!(parent instanceof LinearLayout)) {
            FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(size(pr.opt("width"), ViewGroup.LayoutParams.MATCH_PARENT), size(pr.opt("height"), ViewGroup.LayoutParams.WRAP_CONTENT));
            lp.setMargins(m, m, m, m);
            return lp;
        }
        float weight = (float) pr.optDouble("grow", "Spacer".equals(child.kind) ? 1 : 0);
        if (weight > 0) {
            if (vertical && !pr.has("height")) h = 0;
            if (!vertical && !pr.has("width")) w = 0;
        }
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(w, h, weight);
        lp.setMargins(m + (vertical ? 0 : gap), m + (vertical ? gap : 0), m, m);
        return lp;
    }

    private int size(Object v, int fallback) {
        if (v instanceof Number) return dp(((Number) v).doubleValue());
        if ("100%".equals(v)) return ViewGroup.LayoutParams.MATCH_PARENT;
        return fallback;
    }

    private static int gravity(boolean vertical, String align, String justify) {
        int main;
        switch (justify) {
            case "center":
                main = vertical ? Gravity.CENTER_VERTICAL : Gravity.CENTER_HORIZONTAL;
                break;
            case "end":
                main = vertical ? Gravity.BOTTOM : Gravity.END;
                break;
            default:
                main = vertical ? Gravity.TOP : Gravity.START;
        }
        int cross;
        switch (align) {
            case "center":
                cross = vertical ? Gravity.CENTER_HORIZONTAL : Gravity.CENTER_VERTICAL;
                break;
            case "end":
                cross = vertical ? Gravity.END : Gravity.BOTTOM;
                break;
            default:
                cross = vertical ? Gravity.START : Gravity.TOP;
        }
        return main | cross;
    }

    // ---------- automation ----------

    /** Adds bounds (dp, on screen) and visibility to an inspect tree, like the web host does. */
    void addBounds(JSONArray tree) throws JSONException {
        for (int i = 0; i < tree.length(); i++) {
            JSONObject n = tree.getJSONObject(i);
            Node node = nodes.get(n.getInt("id"));
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
}
