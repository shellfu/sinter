import pytest

import flask


@pytest.mark.parametrize("name", [" api", "api ", " api ", "\tapi"])
def test_whitespace_name_rejected(name):
    with pytest.raises(ValueError):
        flask.Blueprint(name, __name__)


def test_inner_whitespace_still_allowed():
    flask.Blueprint("my api", __name__)
