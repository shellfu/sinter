import flask


def test_category_filter_accepts_str(app):
    @app.route("/")
    def index():
        flask.flash("bad", "error")
        flask.flash("ok", "info")
        flask.flash("x", "o")
        return ",".join(flask.get_flashed_messages(category_filter="error"))

    rv = app.test_client().get("/")
    assert rv.data == b"bad"
