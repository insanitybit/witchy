// Load client: C connections, each doing R round trips of a P-byte payload, every echo
// checked byte for byte. Prints: total seconds, round trips per second, p50 and p99 in us.
package main

import (
	"bytes"
	"fmt"
	"io"
	"net"
	"os"
	"sort"
	"strconv"
	"sync"
	"time"
)

func main() {
	address := os.Args[1]
	connections, _ := strconv.Atoi(os.Args[2])
	rounds, _ := strconv.Atoi(os.Args[3])
	size, _ := strconv.Atoi(os.Args[4])
	payload := make([]byte, size)
	for i := range payload { payload[i] = byte(i % 251) }
	var wait sync.WaitGroup
	latencies := make([][]time.Duration, connections)
	conns := make([]net.Conn, connections)
	for c := 0; c < connections; c++ {
		var err error
		for attempt := 0; ; attempt++ {
			conns[c], err = net.Dial("tcp", address)
			if err == nil { break }
			if attempt > 200 { fmt.Fprintln(os.Stderr, err); os.Exit(1) }
			time.Sleep(10 * time.Millisecond)
		}
	}
	start := time.Now()
	failed := false
	var mu sync.Mutex
	for c := 0; c < connections; c++ {
		wait.Add(1)
		go func(c int) {
			defer wait.Done()
			got := make([]byte, size)
			for r := 0; r < rounds; r++ {
				t := time.Now()
				if _, err := conns[c].Write(payload); err != nil { mu.Lock(); failed = true; mu.Unlock(); return }
				if _, err := io.ReadFull(conns[c], got); err != nil || !bytes.Equal(got, payload) { mu.Lock(); failed = true; mu.Unlock(); return }
				latencies[c] = append(latencies[c], time.Since(t))
			}
			conns[c].Close()
		}(c)
	}
	wait.Wait()
	elapsed := time.Since(start)
	if failed { fmt.Fprintln(os.Stderr, "an echo failed or differed"); os.Exit(1) }
	var all []time.Duration
	for _, l := range latencies { all = append(all, l...) }
	sort.Slice(all, func(i, j int) bool { return all[i] < all[j] })
	fmt.Printf("%.3f %.0f %.1f %.1f\n", elapsed.Seconds(), float64(len(all))/elapsed.Seconds(),
		float64(all[len(all)/2].Microseconds()), float64(all[len(all)*99/100].Microseconds()))
}
