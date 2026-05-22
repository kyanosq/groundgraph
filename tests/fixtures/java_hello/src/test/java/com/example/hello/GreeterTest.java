package com.example.hello;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

class GreeterTest {

    @Test
    void greetsCasuallyByDefault() {
        Greeter g = Greeter.make("Ada");
        assertEquals("Hello, Ada!", g.greet());
    }

    @Test
    void supportsFormalGreeting() {
        Greeter g = new Greeter("Grace", true);
        assertEquals("Good day, Grace!", g.greet());
    }

    @ParameterizedTest
    @ValueSource(strings = {"Linus", "Hopper"})
    void saysGoodbye(String name) {
        assertTrue(new Greeter(name).goodbye().endsWith("!"));
    }
}
