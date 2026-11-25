// Echo server: a goroutine per connection; stops after N connections have closed.
package main

import (
	"fmt"
	"net"
	"os"
	"strconv"
	"sync"
	"sync/atomic"
)

func main() {
	listener, err := net.Listen("tcp", os.Args[1])
	if err != nil { panic(err) }
	n, _ := strconv.Atoi(os.Args[2])
	var total int64
	var wait sync.WaitGroup
	for i := 0; i < n; i++ {
		c, err := listener.Accept()
		if err != nil { panic(err) }
		wait.Add(1)
		go func(c net.Conn) {
			defer wait.Done()
			buffer := make([]byte, 16384)
			for {
				k, err := c.Read(buffer)
				if k > 0 { c.Write(buffer[:k]); atomic.AddInt64(&total, int64(k)) }
				if err != nil { break }
			}
			c.Close()
		}(c)
	}
	wait.Wait()
	fmt.Println(total)
}
