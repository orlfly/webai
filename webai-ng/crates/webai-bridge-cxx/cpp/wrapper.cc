// webai-bridge-cxx: implementation of the C ABI bridge used by cxx.
//
// All WPE / cog / GLib symbols are forward-declared here. The real
// declarations live in the cogcore, libwpe-1.0, and wpe-webkit-2.0
// headers, which `pkg-config` provides. Linking pulls them in via the
// build-system glue in `build.rs`.
//
// Design:
//   The bridge owns a dedicated OS thread that becomes the WebKit
//   "UI thread" / main thread. WPE/WebKit's `isMainThread()` and the
//   per-`WebsiteDataStore` `isUIThread()` assertions both require
//   `WTF::initializeMainThread()` to be called on the thread that
//   creates the very first WebKit object (`WebKitWebContext`).
//   `webkitInitialize()` performs that registration via its
//   `std::call_once` guard the first time any GLib type from WebKit is
//   instantiated; the FIRST `g_object_new(WEBKIT_TYPE_*, ...)` on a
//   thread then "wins" the UI thread slot. So we must construct the
//   shell, the web context, and the `WebKitWebView` on the same thread,
//   and that thread must be the one that runs the GLib main loop
//   thereafter.
//
//   Rust callers push work onto the loop thread via
//   `g_main_context_invoke` (which queues a source on the loop's
//   `GMainContext` and blocks until the loop thread fires it). The loop
//   thread runs a poll loop continuously, so it pulls both the
//   async-result callbacks (delivered by the WebKit child process) and
//   the invoke sources from the same context. Outside threads wait on a
//   `std::condition_variable` for the result of the most recent script.
//
//   This wrapper is deliberately THIN: it only does cog/WPE lifecycle
//   and API forwarding. It implements no browser command logic — all
//   browser behaviour is expressed as JavaScript (定论一).

#include "wrapper.h"

#include <cassert>
#include <cstdlib>
#include <cstring>
#include <string>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <functional>
#include <mutex>
#include <thread>

#include <glib.h>

// Forward-declare the GLib / cog / WPE WebKit symbols that we use. They
// are provided by cogcore, libwpe-1.0, and wpe-webkit-2.0; linking pulls
// in their headers via pkg-config.
struct _JSCValue;
struct _GObject;
struct _GAsyncResult;
struct _GError;
struct _GMainContext;
struct _GMainLoop;

typedef struct _JSCValue JSCValue;
typedef struct _GObject GObject;
typedef struct _GAsyncResult GAsyncResult;
typedef struct _GError GError;
typedef struct _GMainContext GMainContext;
typedef struct _GMainLoop GMainLoop;
typedef struct _WebKitSettings WebKitSettings;

typedef int gboolean;

typedef enum {
    WEBKIT_LOAD_STARTED,
    WEBKIT_LOAD_REDIRECTED,
    WEBKIT_LOAD_COMMITTED,
    WEBKIT_LOAD_FINISHED
} WebKitLoadEvent;

typedef enum {
    WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES,
    WEBKIT_USER_CONTENT_INJECT_TOP_FRAME
} WebkitUserContentInject;

typedef enum {
    WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START,
    WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_END
} WebkitUserScriptInjectTiming;

extern "C" {
    CogShell *cog_shell_new(const char *name, int headless);
    void cog_init(const char *platform_name, const char *module_path);
    CogPlatform *cog_platform_get(void);
    int cog_platform_setup(
        CogPlatform *platform,
        CogShell *shell,
        const char *params,
        GError **error
    );
    void cog_shell_startup(CogShell *shell);
    void cog_shell_shutdown(CogShell *shell);
    WebkitWebContext *cog_shell_get_web_context(CogShell *shell);
    WebKitSettings *cog_shell_get_web_settings(CogShell *shell);
    gboolean cog_shell_is_automated(CogShell *shell);
    WebkitWebView *cog_view_new(const char *first_property, ...);
    void cog_platform_init_web_view(CogPlatform *platform, WebkitWebView *view);

    void g_object_set(void *object, const char *first_property_name, ...);

    void webkit_web_view_load_uri(WebkitWebView *view, const char *uri);
    void webkit_web_view_evaluate_javascript(
        WebkitWebView *view,
        const char *script,
        long length,
        const char *world_name,
        const char *source_uri,
        void *cancellable,
        void *callback,
        void *user_data
    );
    void webkit_web_view_call_async_javascript_function(
        WebkitWebView *view,
        const char *body,
        long length,
        void *arguments,
        const char *world_name,
        const char *source_uri,
        void *cancellable,
        void *callback,
        void *user_data
    );
    WebkitUserContentManager *webkit_web_view_get_user_content_manager(WebkitWebView *view);
    WebKitUserScript *webkit_user_script_new(
        const char *script,
        WebkitUserContentInject inject,
        WebkitUserScriptInjectTiming timing,
        const char *world_name,
        const char **allowed_list
    );
    void webkit_user_content_manager_add_script(
        WebkitUserContentManager *manager,
        WebKitUserScript *script
    );
    char *jsc_value_to_string(JSCValue *value);

    void *webkit_web_view_evaluate_javascript_finish(
        WebkitWebView *view,
        void *result,
        void **error_out
    );
    char *jsc_value_to_json(JSCValue *value);
    void g_free(void *mem);

    void webkit_web_view_get_snapshot(
        WebkitWebView *view,
        int region,
        int options,
        void *cancellable,
        void *callback,
        void *user_data
    );
    void *webkit_web_view_get_snapshot_finish(
        WebkitWebView *view,
        void *result,
        void **error_out
    );
    int webkit_image_get_width(void *image);
    int webkit_image_get_height(void *image);
    unsigned int webkit_image_get_stride(void *image);
    GBytes *webkit_image_as_bytes(void *image);
    void *cairo_image_surface_create_for_data(
        unsigned char *data,
        int format,
        int width,
        int height,
        int stride
    );
    int cairo_surface_write_to_png(void *surface, const char *filename);
    void cairo_surface_destroy(void *surface);

    // Load-finished callback registration. The C++ side connects a
    // `load-changed` signal handler on the view and fires the Rust
    // callback when `WEBKIT_LOAD_FINISHED` is observed.
    void *g_signal_connect_data(
        void *instance,
        const char *detailed_signal,
        void *c_handler,
        void *data,
        void *destroy_data,
        int connect_flags
    );
    const char *webkit_web_view_get_uri(WebkitWebView *view);
    const char *webkit_web_view_get_title(WebkitWebView *view);
}

namespace {

struct WebkitBridge {
    CogShell *shell = nullptr;
    CogPlatform *platform = nullptr;
    WebkitWebContext *context = nullptr;
    WebkitWebView *view = nullptr;
    WebkitUserContentManager *user_content = nullptr;

    // The bridge's dedicated thread. It is the WebKit UI thread.
    void *loop_context = nullptr;
    std::thread loop_thread;
    std::mutex init_mutex;
    std::condition_variable init_cv;
    bool init_ready = false;
    bool init_failed = false;
    std::atomic<bool> quit_requested{false};

    // Result-signalling state for evaluate.
    std::mutex pending_mutex;
    std::condition_variable pending_cv;
    std::string pending_result;
    bool pending_ready = false;

    // Load-finished callback (Rust closure, moved in via rust::Fn).
    std::function<void(const char *, const char *, int)> load_callback;
};

struct PendingCallback {
    WebkitBridge *bridge = nullptr;
};

// Load-changed handler. Fires the Rust trampoline on the loop thread
// when the load reaches WEBKIT_LOAD_FINISHED.
void load_changed_cb(GObject *object, int event, void *user_data) {
    auto *bridge = static_cast<WebkitBridge *>(user_data);
    (void)object;
    if (event != WEBKIT_LOAD_FINISHED) {
        return;
    }
    if (!bridge || !bridge->load_callback) {
        return;
    }
    const char *uri = webkit_web_view_get_uri(bridge->view);
    const char *title = webkit_web_view_get_title(bridge->view);
    bridge->load_callback(
        uri ? uri : "",
        title ? title : "",
        static_cast<int>(WEBKIT_LOAD_FINISHED)
    );
}

// `web_view_javascript_finished` is the GAsyncReadyCallback wired into
// `webkit_web_view_evaluate_javascript`. It calls the JSC-flavoured
// finish function to retrieve the JSCValue result and serialise it as
// JSON, then signals the bridge's wait condition.
void web_view_javascript_finished(
    GObject *object,
    GAsyncResult *result,
    void *user_data
) {
    auto *pending = static_cast<PendingCallback *>(user_data);
    auto *bridge = pending->bridge;
    (void)object;

    GError *raw_error = nullptr;
    auto *js_value = static_cast<JSCValue *>(
        webkit_web_view_evaluate_javascript_finish(
            bridge->view,
            result,
            reinterpret_cast<void **>(&raw_error)
        )
    );

    std::string payload;
    if (!js_value) {
        if (raw_error && raw_error->message) {
            payload = std::string("{\"ok\":false,\"error\":\"")
                    + raw_error->message
                    + "\"}";
        } else {
            payload = "{\"ok\":false,\"error\":\"script returned no value\"}";
        }
    } else {
        char *json = jsc_value_to_json(js_value);
        if (json) {
            payload = std::string("{\"ok\":true,\"value\":") + json + "}";
            g_free(json);
        } else {
            payload = "{\"ok\":true,\"value\":null}";
        }
    }

    if (raw_error) {
        g_error_free(raw_error);
    }

    {
        std::lock_guard<std::mutex> lock(bridge->pending_mutex);
        bridge->pending_result = std::move(payload);
        bridge->pending_ready = true;
    }
    bridge->pending_cv.notify_all();
    delete pending;
}

// Trampolines invoked on the loop thread via `g_main_context_invoke`
// from outside threads. Each is a plain C function so the
// `g_main_context_invoke` signature matches.

struct EvaluateInvokePayload {
    WebkitBridge *bridge;
    std::string *script;
    PendingCallback *pending;
};

static std::mutex g_evaluate_state_mutex;
static WebkitBridge *g_next_bridge = nullptr;
static std::string *g_next_script = nullptr;
static PendingCallback *g_next_pending = nullptr;

struct LoadUriInvokePayload {
    WebkitWebView *view;
    std::string *uri;
};

extern "C" int dispatch_evaluate_invocation(void *arg) {
    (void)arg;
    WebkitBridge *bridge;
    std::string *script;
    PendingCallback *pending;
    {
        std::lock_guard<std::mutex> lock(g_evaluate_state_mutex);
        bridge = g_next_bridge;
        script = g_next_script;
        pending = g_next_pending;
        g_next_bridge = nullptr;
        g_next_script = nullptr;
        g_next_pending = nullptr;
    }
    webkit_web_view_call_async_javascript_function(
        bridge->view,
        script->c_str(),
        -1,
        nullptr,
        nullptr,
        nullptr,
        nullptr,
        reinterpret_cast<void *>(web_view_javascript_finished),
        pending
    );
    return 0; // FALSE = remove the source.
}

extern "C" int dispatch_load_uri_invocation(void *arg) {
    auto *payload = static_cast<LoadUriInvokePayload *>(arg);
    if (!payload || !payload->view || !payload->uri) {
        delete payload;
        return 0;
    }
    WebkitWebView *view = payload->view;
    std::string uri = std::move(*payload->uri);
    delete payload;
    webkit_web_view_load_uri(view, uri.c_str());
    return 0;
}

} // namespace

extern "C" void set_device_scale_factor(void *object, float scale) {
    g_object_set(object, "device-scale-factor", scale, nullptr);
}

WebkitBridge *create_bridge() {
    WebkitBridge *bridge = new WebkitBridge();

    auto *loop_context = static_cast<GMainContext *>(g_main_context_new());
    bridge->loop_context = loop_context;

    bridge->loop_thread = std::thread([bridge, loop_context]() {
        g_main_context_push_thread_default(loop_context);

        GError *error = nullptr;

        bridge->shell = cog_shell_new("shell", 1);
        if (!bridge->shell) {
            bridge->init_failed = true;
            g_main_context_pop_thread_default(loop_context);
            return;
        }
        set_device_scale_factor(bridge->shell, 1.0f);

        const char *platform_name = g_getenv("COG_PLATFORM_NAME");
        if (!platform_name || !*platform_name) {
            platform_name = "headless";
        }
        cog_init(platform_name, NULL);

        bridge->platform = cog_platform_get();
        if (!bridge->platform) {
            bridge->init_failed = true;
            g_main_context_pop_thread_default(loop_context);
            return;
        }

        if (!cog_platform_setup(bridge->platform, bridge->shell, "", &error)) {
            bridge->init_failed = true;
            g_main_context_pop_thread_default(loop_context);
            return;
        }

        cog_shell_startup(bridge->shell);
        bridge->context = cog_shell_get_web_context(bridge->shell);

        WebKitSettings *settings = cog_shell_get_web_settings(bridge->shell);
        if (!settings) {
            bridge->init_failed = true;
            g_main_context_pop_thread_default(loop_context);
            return;
        }
        gboolean automated = cog_shell_is_automated(bridge->shell);
        bridge->view = cog_view_new(
            "settings", settings,
            "web-context", bridge->context,
            "zoom-level", 1.0,
            "is-controlled-by-automation", automated,
            "use-key-bindings", 0,
            (void *)nullptr
        );
        if (!bridge->view) {
            bridge->init_failed = true;
            g_main_context_pop_thread_default(loop_context);
            return;
        }
        cog_platform_init_web_view(bridge->platform, bridge->view);
        bridge->user_content = webkit_web_view_get_user_content_manager(bridge->view);

        // Register the load-changed handler so WEBKIT_LOAD_FINISHED is
        // reported to Rust.
        g_signal_connect_data(
            bridge->view,
            "load-changed",
            reinterpret_cast<void *>(load_changed_cb),
            bridge,
            nullptr,
            0
        );

        {
            std::lock_guard<std::mutex> lock(bridge->init_mutex);
            bridge->init_ready = true;
        }
        bridge->init_cv.notify_all();

        while (true) {
            g_main_context_iteration(loop_context, FALSE);
            if (bridge->quit_requested.load()) {
                break;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
        }
        g_main_context_pop_thread_default(loop_context);
    });

    std::unique_lock<std::mutex> lock(bridge->init_mutex);
    bridge->init_cv.wait_for(lock, std::chrono::seconds(15),
        [&bridge] { return bridge->init_ready || bridge->init_failed; });
    if (bridge->init_failed) {
        bridge->quit_requested.store(true);
        if (bridge->loop_thread.joinable()) {
            bridge->loop_thread.join();
        }
        delete bridge;
        return nullptr;
    }

    return bridge;
}

// cxx expects user-implemented C++ functions with plain names; the
// generated shim looks them up by symbol and stores the pointer in a
// `$`-suffixed function-pointer table.
extern "C" {

WebkitView *webkit_bridge_open() {
    return reinterpret_cast<WebkitView *>(create_bridge());
}

void webkit_bridge_close(WebkitView *view) {
    auto *bridge = reinterpret_cast<WebkitBridge *>(view);
    if (!bridge) {
        return;
    }
    bridge->quit_requested.store(true);
    if (bridge->loop_thread.joinable()) {
        bridge->loop_thread.join();
    }
    if (bridge->loop_context) {
        g_main_context_unref(static_cast<GMainContext *>(bridge->loop_context));
        bridge->loop_context = nullptr;
    }
    if (bridge->shell) {
        cog_shell_shutdown(bridge->shell);
    }
    delete bridge;
}

int32_t webkit_bridge_load_uri(WebkitView *view, ::rust::Str uri) {
    auto *bridge = reinterpret_cast<WebkitBridge *>(view);
    if (!bridge || !bridge->view || !bridge->loop_context) {
        return -1;
    }
    auto *payload = new LoadUriInvokePayload{
        bridge->view,
        new std::string(uri.data(), uri.size()),
    };
    g_main_context_invoke(
        static_cast<GMainContext *>(bridge->loop_context),
        reinterpret_cast<GSourceFunc>(dispatch_load_uri_invocation),
        payload
    );
    return 0;
}

int32_t webkit_bridge_inject_user_script(WebkitView *view, ::rust::Str script) {
    auto *bridge = reinterpret_cast<WebkitBridge *>(view);
    if (!bridge || !bridge->user_content) {
        return -1;
    }
    std::string s(script.data(), script.size());
    char *script_cstr = strdup(s.c_str());
    WebKitUserScript *user_script = webkit_user_script_new(
        script_cstr,
        WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES,
        WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START,
        nullptr,
        nullptr
    );
    if (!user_script) {
        free(script_cstr);
        return -1;
    }
    webkit_user_content_manager_add_script(bridge->user_content, user_script);
    return 0;
}

int32_t webkit_bridge_evaluate_javascript(
    WebkitView *view,
    ::rust::Str script,
    uint32_t timeout_ms,
    int32_t &kind_out,
    ::std::unique_ptr<::std::string> &payload_out
) {
    auto *bridge = reinterpret_cast<WebkitBridge *>(view);
    if (!bridge || !bridge->view || !bridge->loop_context) {
        return -1;
    }

    std::string s(script.data(), script.size());
    auto *script_str = new std::string(std::move(s));
    auto *pending = new PendingCallback{bridge};
    {
        std::lock_guard<std::mutex> lock(bridge->pending_mutex);
        bridge->pending_result.clear();
        bridge->pending_ready = false;
        g_next_bridge = bridge;
        g_next_script = script_str;
        g_next_pending = pending;
    }

    g_main_context_invoke(
        static_cast<GMainContext *>(bridge->loop_context),
        reinterpret_cast<GSourceFunc>(dispatch_evaluate_invocation),
        script_str
    );

    std::unique_lock<std::mutex> lock(bridge->pending_mutex);
    bool got_result = bridge->pending_cv.wait_for(
        lock,
        std::chrono::milliseconds(timeout_ms),
        [&bridge] { return bridge->pending_ready; }
    );

    delete script_str;

    if (!got_result) {
        kind_out = 1;
        return 0;
    }

    kind_out = 0;
    payload_out = std::make_unique<std::string>(bridge->pending_result);
    return 0;
}

int32_t webkit_bridge_resize(WebkitView *view, uint32_t width, uint32_t height) {
    (void)view;
    (void)width;
    (void)height;
    return 0;
}

// Snapshot callback state.
struct SnapshotCallback {
    WebkitBridge *bridge;
    std::string dest_path;
    bool ready = false;
    bool ok = false;
};

void snapshot_finished(GObject *object, GAsyncResult *result, void *user_data) {
    auto *cb = static_cast<SnapshotCallback *>(user_data);
    (void)object;
    GError *raw_error = nullptr;
    auto *image = static_cast<void *>(
        webkit_web_view_get_snapshot_finish(
            cb->bridge->view,
            result,
            reinterpret_cast<void **>(&raw_error)
        )
    );
    bool ok = false;
    if (image) {
        int width = webkit_image_get_width(image);
        int height = webkit_image_get_height(image);
        unsigned int stride = webkit_image_get_stride(image);
        GBytes *bytes = webkit_image_as_bytes(image);
        gsize size = 0;
        gconstpointer data = g_bytes_get_data(bytes, &size);
        if (data && width > 0 && height > 0 && stride > 0) {
            void *surface = cairo_image_surface_create_for_data(
                const_cast<unsigned char *>(static_cast<const unsigned char *>(data)),
                0, // CAIRO_FORMAT_ARGB32
                width,
                height,
                static_cast<int>(stride)
            );
            if (surface) {
                int rc = cairo_surface_write_to_png(surface, cb->dest_path.c_str());
                cairo_surface_destroy(surface);
                ok = (rc == 0);
            }
        }
        g_bytes_unref(bytes);
    }
    if (raw_error) {
        g_error_free(raw_error);
    }
    {
        std::lock_guard<std::mutex> lock(cb->bridge->pending_mutex);
        cb->ok = ok;
        cb->ready = true;
    }
    cb->bridge->pending_cv.notify_all();
}

struct ScreenshotInvoke {
    WebkitBridge *bridge;
    SnapshotCallback *cb;
};

extern "C" int dispatch_screenshot_invocation(void *arg) {
    auto *p = static_cast<ScreenshotInvoke *>(arg);
    webkit_web_view_get_snapshot(
        p->bridge->view,
        0, // WEBKIT_SNAPSHOT_REGION_VISIBLE
        0, // WEBKIT_SNAPSHOT_OPTIONS_NONE
        nullptr,
        reinterpret_cast<void *>(snapshot_finished),
        p->cb
    );
    delete p;
    return 0;
}

int32_t webkit_bridge_screenshot(WebkitView *view, ::rust::Str dest_path) {
    auto *bridge = reinterpret_cast<WebkitBridge *>(view);
    if (!bridge || !bridge->view || !bridge->loop_context) {
        return -1;
    }
    auto *cb = new SnapshotCallback{
        bridge,
        std::string(dest_path.data(), dest_path.size()),
        false,
        false,
    };
    auto *payload = new ScreenshotInvoke{bridge, cb};
    g_main_context_invoke(
        static_cast<GMainContext *>(bridge->loop_context),
        reinterpret_cast<GSourceFunc>(dispatch_screenshot_invocation),
        payload
    );
    std::unique_lock<std::mutex> lock(bridge->pending_mutex);
    auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(15000);
    while (!cb->ready && std::chrono::steady_clock::now() < deadline) {
        bridge->pending_cv.wait_for(lock, std::chrono::milliseconds(50));
    }
    bool ok = cb->ok;
    lock.unlock();
    delete cb;
    if (!ok) {
        return -1;
    }
    return 0;
}

// Register the load-finished callback. The C++ side stores the Rust
// closure (rust::Fn) and calls it on the loop thread when
// `WEBKIT_LOAD_FINISHED` is observed.
void webkit_bridge_set_load_callback(
    WebkitView *view,
    ::rust::Fn<void(const char *, const char *, int)> callback
) {
    auto *bridge = reinterpret_cast<WebkitBridge *>(view);
    if (!bridge) {
        return;
    }
    bridge->load_callback = std::move(callback);
}

} // extern "C"
