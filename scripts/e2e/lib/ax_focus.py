#!/usr/bin/env python3
"""Read an application's *real* focused window through Accessibility.

The e2e scenarios need this because request acceptance and API return codes cannot show that the
exact window got focus: only the app's own AXFocusedWindow can, mapped back to a CGWindowID.

It is a module (imported by the scenario blocks) and a self-testing script
(`python3 -B scripts/e2e/lib/ax_focus.py`), so an unreadable or misdeclared symbol cannot pass
silently as "no window focused".
"""

import ctypes
import sys

_APP_SERVICES = "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices"
_CORE_FOUNDATION = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
_HISERVICES = "/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices"

CF_STRING_UTF8 = 0x08000100
_AX_ERROR_SUCCESS = 0

app_services = ctypes.CDLL(_APP_SERVICES)
core_foundation = ctypes.CDLL(_CORE_FOUNDATION)
hiservices = ctypes.CDLL(_HISERVICES)

app_services.AXUIElementCreateApplication.restype = ctypes.c_void_p
app_services.AXUIElementCreateApplication.argtypes = [ctypes.c_int32]
app_services.AXUIElementCopyAttributeValue.restype = ctypes.c_int32
app_services.AXUIElementCopyAttributeValue.argtypes = [
    ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)
]
app_services.AXUIElementSetMessagingTimeout.restype = ctypes.c_int32
app_services.AXUIElementSetMessagingTimeout.argtypes = [ctypes.c_void_p, ctypes.c_float]
hiservices._AXUIElementGetWindow.restype = ctypes.c_int32
hiservices._AXUIElementGetWindow.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
core_foundation.CFStringCreateWithCString.restype = ctypes.c_void_p
core_foundation.CFStringCreateWithCString.argtypes = [
    ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32
]
core_foundation.CFRelease.argtypes = [ctypes.c_void_p]

# 50ms: a hung app must not stall the scenario, the same bound the app itself uses for AX reads.
_AX_MESSAGING_TIMEOUT_SECONDS = 0.05


def focused_window_id(pid):
    """The CGWindowID of `pid`'s focused window, or None when the read cannot answer.

    `None` covers both "this app reports no focused window" and "the read is not permitted"; a
    caller that needs to tell those apart probes a few apps before judging (see the scenarios).
    """
    key = core_foundation.CFStringCreateWithCString(None, b"AXFocusedWindow", CF_STRING_UTF8)
    if not key:
        return None
    app = app_services.AXUIElementCreateApplication(pid)
    if not app:
        core_foundation.CFRelease(key)
        return None
    try:
        app_services.AXUIElementSetMessagingTimeout(app, _AX_MESSAGING_TIMEOUT_SECONDS)
        value = ctypes.c_void_p()
        if app_services.AXUIElementCopyAttributeValue(app, key, ctypes.byref(value)) != _AX_ERROR_SUCCESS:
            return None
        if not value:
            return None
        try:
            window_id = ctypes.c_uint32(0)
            if hiservices._AXUIElementGetWindow(value, ctypes.byref(window_id)) != _AX_ERROR_SUCCESS:
                return None
            return int(window_id.value)
        finally:
            core_foundation.CFRelease(value)
    finally:
        core_foundation.CFRelease(app)
        core_foundation.CFRelease(key)


def _selftest():
    # A pid that cannot exist must answer None instead of raising or inventing an id -- that is the
    # counter-example that makes this a gate rather than a hopeful read.
    bogus = 999_999
    assert focused_window_id(bogus) is None, "a bogus pid must not produce a window id"
    # A *live* process without a focused window must answer None as well: this is the shape a failed
    # switch takes ("the app is there, its focus window is not"), and it must not raise either.
    assert focused_window_id(1) is None, "a live process without a focused window must answer None"
    # And the call must be repeatable without leaking into the next read.
    assert focused_window_id(bogus) is None, "the second read must answer the same way"
    if len(sys.argv) > 1:
        for raw in sys.argv[1:]:
            print(f"{raw}\t{focused_window_id(int(raw))}")
    print("ax_focus selftest: ok")


if __name__ == "__main__":
    _selftest()
