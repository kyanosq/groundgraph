package com.example.hello;

/**
 * Spring-flavoured controller. We do not depend on Spring itself
 * (keeps the fixture build-free), but use the conventional
 * annotations so framework-aware indexers can still recognise it.
 */
@RestController
@RequestMapping("/api")
public class HelloController {

    private final Greeter greeter;

    public HelloController(Greeter greeter) {
        this.greeter = greeter;
    }

    @GetMapping("/hello")
    public String hello() {
        return greeter.greet();
    }

    @GetMapping("/bye")
    public String bye() {
        return greeter.goodbye();
    }
}

// Stand-in annotation declarations so the fixture compiles on its
// own. Real projects pull these from Spring's classpath.
@interface RestController {}
@interface RequestMapping {
    String value();
}
@interface GetMapping {
    String value();
}
