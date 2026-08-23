import flask


def test_bare_prefix_skipped(monkeypatch):
    monkeypatch.setenv("FLASK_", "1")
    monkeypatch.setenv("FLASK_X", "2")
    app = flask.Flask(__name__)
    app.config.from_prefixed_env()
    assert "" not in app.config
    assert app.config["X"] == 2
