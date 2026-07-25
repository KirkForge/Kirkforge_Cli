// Go method receivers — exercises WO 8.9 edge case 5.
// A method on *Server should be extracted as "Server.Start" (receiver
// type, not the parameter name), and a method on Server (value receiver)
// should also be extracted as "Server.Stop".
package main

type Server struct {
	host string
}

func (s *Server) Start() {
}

func (r Server) Stop() {
}

func plain() {
}
