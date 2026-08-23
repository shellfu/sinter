from flask.helpers import get_debug_flag


def test_off_is_false(monkeypatch):
    for v in ("off", "OFF", "0", "false", "no"):
        monkeypatch.setenv("FLASK_DEBUG", v)
        assert get_debug_flag() is False
    monkeypatch.setenv("FLASK_DEBUG", "1")
    assert get_debug_flag() is True
