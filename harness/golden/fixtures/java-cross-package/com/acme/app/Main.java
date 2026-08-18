package com.acme.app;

import com.acme.util.Text;

/** Uses the shared text helpers. */
public class Main {
    /** Runs the app. */
    public String run(String raw) {
        return Text.trim(raw);
    }
}
