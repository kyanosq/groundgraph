package main

import (
	"fmt"

	"example.com/hello/internal/api"
)

func main() {
	srv := api.NewServer("Greeter")
	fmt.Println(srv.Greet())
}
