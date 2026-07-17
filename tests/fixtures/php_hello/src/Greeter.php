<?php
namespace App;

class Greeter
{
    // Same-file Calls edge: greet() calls format().
    public function greet(string $name): string
    {
        return $this->format($name);
    }

    private function format(string $name): string
    {
        // Cross-file call: format() calls the free function salutation()
        // defined in helpers.php (same namespace).
        return salutation() . ", " . $name;
    }
}
