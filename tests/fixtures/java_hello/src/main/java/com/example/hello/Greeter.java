package com.example.hello;

/** Tiny greeter type used by the GroundGraph Java indexer fixture. */
public class Greeter {

    private final String name;
    private final boolean formal;

    public Greeter(String name) {
        this(name, false);
    }

    public Greeter(String name, boolean formal) {
        this.name = name;
        this.formal = formal;
    }

    public String greet() {
        String prefix = formal ? "Good day" : "Hello";
        return prefix + ", " + name + "!";
    }

    public String goodbye() {
        return "Bye, " + name + "!";
    }

    public static Greeter make(String name) {
        return new Greeter(name);
    }
}
