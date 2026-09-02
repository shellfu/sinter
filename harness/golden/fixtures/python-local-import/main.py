def first(p):
    from pkg.paths import nfc
    return nfc(p)


def second(p):
    from pkg.paths import nfc
    return nfc(nfc(p))
