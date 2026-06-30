package main

import "fmt"

func main() {
	ch := make(chan int, 64)
	go func() {
		for i := 0; i < 8000; i++ {
			ch <- i
		}
		close(ch)
	}()
	n := 0
	for range ch {
		n++
	}
	fmt.Println(n)
}
