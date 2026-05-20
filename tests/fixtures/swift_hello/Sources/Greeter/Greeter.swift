import Foundation

public struct GreeterConfig {
    public let name: String
    public init(name: String) {
        self.name = name
    }
}

public protocol GreeterAPI {
    func greet() -> String
}

public final class Greeter: GreeterAPI {
    private let config: GreeterConfig

    public init(config: GreeterConfig) {
        self.config = config
    }

    public func greet() -> String {
        return "Hello, \(config.name)"
    }

    public func goodbye() -> String {
        return "Bye, \(config.name)"
    }
}

public enum GreeterKind {
    case casual
    case formal
}

public func makeGreeter(named name: String) -> Greeter {
    return Greeter(config: GreeterConfig(name: name))
}
