// Compatibility shims for WPE WebKit 2.48 on this build host.
//
// wrapper.cc (and the cxx-generated shim) reference the WebKitGTK-style
// snapshot API which does not exist in WPE builds at all:
//   webkit_web_view_get_snapshot / _finish / webkit_image_*
// The stubs return null so the screenshot verb surfaces a structured
// error instead of crashing. All symbols must be extern "C" so the
// object names match the plain C symbols referenced by the wrapper.
#include <glib.h>

extern "C" {

void* webkit_web_view_get_snapshot(void*, int, int, void*, void*, void*) {
    return nullptr;
}

void* webkit_web_view_get_snapshot_finish(void*, void*, void**) {
    return nullptr;
}

int webkit_image_get_width(void*) { return 0; }

int webkit_image_get_height(void*) { return 0; }

unsigned int webkit_image_get_stride(void*) { return 0; }

GBytes* webkit_image_as_bytes(void*) { return nullptr; }

}  // extern "C"