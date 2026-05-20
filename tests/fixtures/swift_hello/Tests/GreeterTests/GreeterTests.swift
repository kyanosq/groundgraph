import XCTest
@testable import Greeter

final class GreeterTests: XCTestCase {
    func testGreetsByName() {
        let greeter = makeGreeter(named: "World")
        XCTAssertEqual(greeter.greet(), "Hello, World")
    }
}
