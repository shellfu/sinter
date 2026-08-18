package com.acme.greet;

/** Console implementation of Greeter. */
public class ConsoleGreeter implements Greeter {
    /** Greets with a plain prefix. */
    public String greet(String name) {
        return "hi " + name;
    }
}
