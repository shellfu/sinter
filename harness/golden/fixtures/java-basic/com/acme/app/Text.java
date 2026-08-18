package com.acme.app;

/** String helpers. */
public class Text {
    /** Maximum line length. */
    public static final int MAX_LEN = 80;

    /** Trims surrounding whitespace. */
    public static String trim(String input) {
        return input.strip();
    }
}
