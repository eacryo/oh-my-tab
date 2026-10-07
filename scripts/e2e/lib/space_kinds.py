"""Which windows live on a fullscreen Space, for the Space scenarios.

Since 2026-10-07 the contract shows every fullscreen Space's window whatever the "other desktops"
switch says (a fullscreen window has no AX element while it is on its own Space, so its reachability
cannot depend on the switch). Scenarios need the same set to mirror the exemption in the
"AX named another window, so this one is a helper" rule.

`python3 scripts/e2e/lib/space_kinds.py` runs the regression cases.
"""

from __future__ import annotations


def fullscreen_window_ids(spaces, windows):
    """Window ids whose Space list contains a Space the app typed as fullscreen.

    `spaces` is the app's diagnostic list (`[{"id": …, "kind": "ordinary"|"fullscreen"|"unknown"}]`)
    and `windows` a WindowServer window list carrying `window_id` and `space_ids`.
    """
    kinds = {space["id"]: space["kind"] for space in (spaces or [])}
    if not kinds:
        return set()
    return {
        window["window_id"]
        for window in windows
        if any(kinds.get(space) == "fullscreen" for space in (window.get("space_ids") or []))
    }


def fullscreen_current(contexts):
    """Whether any display's current Space is a fullscreen Space (the contract then shows everything).

    Phase 1's "the ordinary cross-desktop window is not a card" expectation only holds while this is
    false, so the scenario has to read it instead of assuming it.
    """
    return any((context or {}).get("kind") == "fullscreen" for context in contexts or [])


def pick_fullscreen_target(windows, fullscreen_ids):
    """A titled, substantial window on a fullscreen Space, or None when the machine has none.

    Pinning one exact window is what makes the regression check meaningful: "some fullscreen card
    exists" cannot notice one of several fullscreen windows disappearing.
    """
    for window in windows or []:
        if window.get("window_id") not in fullscreen_ids:
            continue
        bounds = window.get("bounds") or {}
        if window.get("title") and bounds.get("width", 0) >= 200 and bounds.get("height", 0) >= 200:
            return window
    return None


def target_rejection_reason(frame, window_id):
    """Why the app refused this window a card, as the app itself recorded it.

    `ax_excluded` is the accepted narrowing (the app's AX answer named a window on this window's own
    Space and never named it, so its desktop has not been visited in this process). Every other reason
    -- `space`, `title`, `shape`, `unpaired` -- means admission refused a window it should have
    admitted, and the scenario must keep failing. `None` means the app never reached a decision about
    it at all, which is also not an unrun premise.
    """
    for entry in frame.get("rejected_windows") or []:
        if entry.get("window_id") == window_id:
            return entry.get("reason")
    return None


def _selftest() -> None:
    spaces = [
        {"id": 1, "kind": "ordinary"},
        {"id": 386, "kind": "ordinary"},
        {"id": 430, "kind": "fullscreen"},
        {"id": 999, "kind": "unknown"},
    ]
    windows = [
        {"window_id": 10, "space_ids": [1]},
        {"window_id": 11, "space_ids": [430]},
        {"window_id": 12, "space_ids": [1, 430]},  # sticky: on both
        {"window_id": 13, "space_ids": [386]},
        {"window_id": 14, "space_ids": [999]},  # an unknown kind is not fullscreen
        {"window_id": 15, "space_ids": []},
        {"window_id": 16},
    ]
    assert fullscreen_window_ids(spaces, windows) == {11, 12}, fullscreen_window_ids(spaces, windows)
    # No diagnostic list means no evidence, never "everything is fullscreen".
    assert fullscreen_window_ids([], windows) == set()
    assert fullscreen_window_ids(None, windows) == set()
    # The caller branch: only the app's own `ax_excluded` reason makes a missing target unrun.
    frame = {"rejected_windows": [{"window_id": 20, "reason": "ax_excluded"}]}
    assert target_rejection_reason(frame, 20) == "ax_excluded"
    assert target_rejection_reason(frame, 21) is None
    # Every other recorded reason is a real failure, not an unrun premise.
    for reason in ("space", "title", "shape", "unpaired"):
        assert target_rejection_reason({"rejected_windows": [{"window_id": 20, "reason": reason}]}, 20) == reason
    # No rejection list at all (an older build, or the app never decided): never unrun.
    assert target_rejection_reason({}, 20) is None
    assert target_rejection_reason({"rejected_windows": []}, 20) is None
    # The two premise branches of phase 1, with fixed inputs.
    assert fullscreen_current([{"kind": "ordinary"}, {"kind": "ordinary"}]) is False
    assert fullscreen_current([{"kind": "ordinary"}, {"kind": "fullscreen"}]) is True
    assert fullscreen_current([]) is False
    assert fullscreen_current(None) is False
    windows = [
        {"window_id": 1, "title": "desktop window", "bounds": {"width": 900, "height": 700}},
        {"window_id": 2, "title": "", "bounds": {"width": 900, "height": 700}},
        {"window_id": 3, "title": "tiny", "bounds": {"width": 40, "height": 40}},
        {"window_id": 4, "title": "fullscreen", "bounds": {"width": 1440, "height": 900}},
    ]
    assert pick_fullscreen_target(windows, {2, 3, 4})["window_id"] == 4
    # No candidate on a fullscreen Space: the premise is missing, so the check is unrun, not failed.
    # (Windows 2 and 3 are on fullscreen Spaces but have no title / are too small.)
    assert pick_fullscreen_target(windows, {2, 3}) is None
    assert pick_fullscreen_target([], {4}) is None
    print("space_kinds selftest: ok")


if __name__ == "__main__":
    _selftest()
