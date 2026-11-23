package com.mindmakr.tinyjump;

import android.content.Context;
import android.view.View;
import android.view.ViewGroup;
import org.json.JSONObject;

/**
 * A custom native Android View registered under a name (e.g. {@code "MapView"}) via
 * {@link TinyJump#registerComponent(String, NativeComponent)} and used from {@code .tj} as
 * {@code <Native name="MapView" ... />}.
 */
public interface NativeComponent {
    /** Sends a UI event from the native view back into the tinyjump runtime. */
    interface EventSink {
        void emit(String event, Object value);
    }

    /** Creates the native view with its initial props. */
    View create(Context context, JSONObject props, EventSink emit);

    /** Applies changed props (with the full current props map) on re-render. */
    void update(View view, JSONObject props, JSONObject changed);

    /** Optional container for child views; return null for leaf native views. */
    default ViewGroup container(View view) {
        return null;
    }

    /** Optional cleanup when the node is removed from the view tree. */
    default void destroy(View view) {}
}
