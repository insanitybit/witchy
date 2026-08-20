package main

import (
	"fmt"
	"sync"
	"time"
)

const perSource = 2048

func produce(tx chan<- int, start int, done *sync.WaitGroup) {
	defer done.Done()
	for i := 0; i < perSource; i++ {
		tx <- start + i*2
	}
}

// receivePriority matches chan.select's deterministic tie rule: if both
// receivers are ready, the first wins. The second select blocks only when the
// first receiver is empty.
func receivePriority(a, b <-chan int) (int, int) {
	select {
	case value := <-a:
		return 0, value
	default:
	}
	select {
	case value := <-a:
		return 0, value
	case value := <-b:
		return 1, value
	}
}

func expectedChecksum() int {
	checksum := 0
	for i := 0; i < perSource; i++ {
		checksum = (checksum*257 + i*2 + 1) % 1000000007
	}
	for i := 0; i < perSource; i++ {
		checksum = (checksum*257 + i*2 + 3) % 1000000007
	}
	return checksum
}

func main() {
	a := make(chan int, perSource)
	b := make(chan int, perSource)
	var producers sync.WaitGroup
	producers.Add(2)
	go produce(a, 0, &producers)
	go produce(b, 1, &producers)
	producers.Wait()

	seenA, seenB := 0, 0
	checksum := 0
	orderOK := true
	t0 := time.Now()
	for seenA+seenB < perSource*2 {
		source, value := receivePriority(a, b)
		if source == 0 {
			if seenB != 0 || value != seenA*2 {
				orderOK = false
			}
			checksum = (checksum*257 + value + 1) % 1000000007
			seenA++
		} else {
			if seenA != perSource || value != seenB*2+1 {
				orderOK = false
			}
			checksum = (checksum*257 + value + 2) % 1000000007
			seenB++
		}
	}
	benchNS := time.Since(t0).Nanoseconds()

	expected := expectedChecksum()
	if orderOK && seenA == perSource && seenB == perSource && checksum == expected {
		fmt.Printf("count=%d checksum=%d order=ok\n", seenA+seenB, checksum)
	} else {
		fmt.Printf("count=%d checksum=%d expected=%d order=bad\n", seenA+seenB, checksum, expected)
	}
	fmt.Printf("bench_ns=%d\n", benchNS)
}
