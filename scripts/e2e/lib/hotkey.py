"""The global switcher chord, read from the user's config.

The A2 scenarios inject the real global hotkey, and the modifier is a user setting
(`keyboard.modifier`, validated by the app as "option" or "command", default "command"). Hard-coding
one chord makes a scenario silently inject the wrong key on any machine configured with the other --
the app never summons, and the failure looks like a product bug rather than a harness bug. This helper
derives the chord from the config instead, and fails loudly on a value it does not know rather than
guessing.

`python3 scripts/e2e/lib/hotkey.py` runs the regression cases.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

# kCGEventFlagMaskAlternate / kCGEventFlagMaskCommand, and the kVK_* key codes.
_OPTION = (0x80000, 58)
_COMMAND = (0x100000, 55)
_TAB_KEYCODE = 48
_DEFAULT_MODIFIER = "command"


@dataclass(frozen=True)
class Chord:
    """The modifier flag, the modifier key code, and the Tab key code of the switcher chord."""

    flag: int
    modifier_keycode: int
    tab_keycode: int = _TAB_KEYCODE


def modifier_value(text: str, default: str = _DEFAULT_MODIFIER) -> str:
    """The `keyboard.modifier` value in a config file's text, or `default` when it is absent."""
    in_keyboard = False
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            in_keyboard = line[1:-1].strip() == "keyboard"
            continue
        if not in_keyboard:
            continue
        key, _, value = line.partition("=")
        if key.strip() == "modifier":
            return value.strip().strip('"').strip("'").lower()
    return default


def chord_for_modifier(modifier: str) -> Chord:
    if modifier == "option":
        flag, keycode = _OPTION
    elif modifier == "command":
        flag, keycode = _COMMAND
    else:
        raise ValueError(f"unsupported keyboard.modifier value: {modifier!r}")
    return Chord(flag=flag, modifier_keycode=keycode)


def chord(config_path: str | Path) -> Chord:
    """The chord the app is configured for. Raises when the config is unreadable or unsupported."""
    text = Path(config_path).read_text(encoding="utf-8", errors="replace")
    return chord_for_modifier(modifier_value(text))


def _selftest() -> None:
    # Both supported values, the default when the section is missing, and rejection of anything else.
    assert chord_for_modifier("option") == Chord(0x80000, 58, 48)
    assert chord_for_modifier("command") == Chord(0x100000, 55, 48)
    assert modifier_value('[keyboard]\nmodifier = "command"\n') == "command"
    assert modifier_value('[keyboard]\nmodifier = "option"\n') == "option"
    assert modifier_value("[keyboard]\n") == "command", "an absent key uses the app default"
    assert modifier_value('[other]\nmodifier = "option"\n') == "command", "only [keyboard] counts"
    assert (
        modifier_value('[keyboard]\nmodifier = "option"  # the user\'s choice\n') == "option"
    ), "a trailing comment is not part of the value"
    try:
        chord_for_modifier("ctrl")
    except ValueError:
        pass
    else:
        raise AssertionError("an unsupported modifier must raise, not fall back to a chord")
    print("hotkey selftest: ok")


if __name__ == "__main__":
    _selftest()
