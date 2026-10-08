package main
import("fmt";"time";"strconv")
type packet struct{i int;text string}
func main(){const n=__MESSAGES__;t0:=time.Now();ch:=make(chan packet,64);go func(){for i:=0;i<n;i++{ch<-packet{i,"payload "+strconv.Itoa(i)}};close(ch)}();sum:=0;expected:=0;for p:=range ch{if p.i!=expected||p.text!="payload "+strconv.Itoa(p.i){panic("payload/FIFO mismatch")};expected++;sum+=p.i+len(p.text)};elapsed:=time.Since(t0).Nanoseconds();fmt.Println(sum);fmt.Printf("bench_ns=%d\n",elapsed)}
