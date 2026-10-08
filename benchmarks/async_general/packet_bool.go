package main
import("fmt";"time")
type packet struct{i int;flag bool}
func main(){const n=__MESSAGES__;t0:=time.Now();ch:=make(chan packet,64);go func(){for i:=0;i<n;i++{ch<-packet{i,i%2==0}};close(ch)}();sum:=0;expected:=0;for p:=range ch{if p.i!=expected||p.flag!=(p.i%2==0){panic("payload/FIFO mismatch")};expected++;sum+=p.i};elapsed:=time.Since(t0).Nanoseconds();fmt.Println(sum);fmt.Printf("bench_ns=%d\n",elapsed)}
