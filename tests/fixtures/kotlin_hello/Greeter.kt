package app

// Same-file Calls edge: greet() calls format(). Cross-file: format() touches
// the Registry object defined in Registry.kt (same package).
class Greeter(private val name: String) {
    fun greet(): String {
        return format()
    }

    private fun format(): String {
        return Registry.prefix() + name
    }
}
