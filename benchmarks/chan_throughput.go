package main

import "fmt"

func main() {
	const n = 64000
	ch := make(chan int, 64)
	go func() {
		for i := 0; i < n; i++ {
			ch <- i
		}
		close(ch)
	}()
	sum := 0
	for v := range ch {
		sum += v
	}
	fmt.Println(sum)
}
