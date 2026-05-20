package api

// Greeter is satisfied by any greeting backend.
type Greeter interface {
	Greet() string
}

// Server is a tiny domain object that holds a greeter name and the
// methods exercised by the SpecSlice Go indexer fixture.
type Server struct {
	Name string
}

// NewServer constructs a Server bound to the given name.
func NewServer(name string) *Server {
	return &Server{Name: name}
}

// Greet returns the canonical greeting for this server.
func (s *Server) Greet() string {
	return "hello, " + s.Name
}

// Goodbye returns the canonical farewell for this server.
func (s *Server) Goodbye() string {
	return "bye, " + s.Name
}
