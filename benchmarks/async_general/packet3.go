package main
import("fmt";"time")
type packet struct { values [3]int }
func main(){const n=__MESSAGES__;t0:=time.Now();ch:=make(chan packet,64);go func(){for i:=0;i<n;i++{p:=packet{};for j:=range p.values{p.values[j]=i+j};ch<-p};close(ch)}();sum:=0;for p:=range ch{for _,v:=range p.values{sum+=v}};elapsed:=time.Since(t0).Nanoseconds();fmt.Println(sum);fmt.Printf("bench_ns=%d\n",elapsed)}
