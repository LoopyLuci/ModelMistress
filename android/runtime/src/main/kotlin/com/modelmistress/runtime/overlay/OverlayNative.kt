package com.modelmistress.runtime.overlay

/**
 * The JNI functions of the Transferd overlay node, built from `android/overlay-jni` (crate `mm-overlay-jni`). Each one
 * answers with a JSON object: `{"ok": true, ...}` or `{"ok": false, "error": "..."}`. Use [TransferdOverlay], which reads
 * those answers; do not call these from the interface.
 *
 * Loading the library fails with a [LinkageError] in a build that has no overlay library. [TransferdOverlay] reports that.
 */
internal object OverlayNative {
    init {
        System.loadLibrary("mm_overlay_jni")
    }

    /** Starts this process's node, or returns the running one. `relays` and `direct` are comma-separated. */
    external fun start(home: String, name: String, relays: String, direct: String, listen: String): String?

    /** A pairing invite, valid for [ttlSeconds] while the node runs. */
    external fun invite(ttlSeconds: Long): String?

    /** Pairs with the node that made [invite]. [name] is the label to know it by. */
    external fun accept(invite: String, name: String): String?

    /** A loopback address that reaches [service] on the paired node [nodeId]. */
    external fun connect(nodeId: String, service: String): String?

    /** Whether the node is running, with its id, listener, relays and peers. */
    external fun status(): String?
}
