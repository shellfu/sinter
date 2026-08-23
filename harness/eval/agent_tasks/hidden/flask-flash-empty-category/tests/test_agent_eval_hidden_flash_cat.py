import flask


def test_empty_category_defaults(app):
    @app.route("/")
    def index():
        flask.flash("hi", "")
        msgs = flask.get_flashed_messages(with_categories=True)
        return repr(msgs)

    rv = app.test_client().get("/")
    assert rv.data == repr([("message", "hi")]).encode()
