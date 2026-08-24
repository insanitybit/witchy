package main

import (
	"fmt"
	"math"
	"time"
)

const (
	Pi   = 3.141592653589793
	Days = 365.24
	SolarMass = 4 * Pi * Pi
)

type Body struct {
	x, y, z, vx, vy, vz, mass float64
}

func advance(bodies []Body, dt float64) {
	for i := 0; i < len(bodies); i++ {
		b := &bodies[i]
		for j := i + 1; j < len(bodies); j++ {
			b2 := &bodies[j]
			dx := b.x - b2.x
			dy := b.y - b2.y
			dz := b.z - b2.z
			distance := math.Sqrt(dx*dx + dy*dy + dz*dz)
			mag := dt / (distance * distance * distance)
			
			b.vx -= dx * b2.mass * mag
			b.vy -= dy * b2.mass * mag
			b.vz -= dz * b2.mass * mag
			
			b2.vx += dx * b.mass * mag
			b2.vy += dy * b.mass * mag
			b2.vz += dz * b.mass * mag
		}
	}
	
	for i := 0; i < len(bodies); i++ {
		b := &bodies[i]
		b.x += dt * b.vx
		b.y += dt * b.vy
		b.z += dt * b.vz
	}
}

func energy(bodies []Body) float64 {
	e := 0.0
	for i := 0; i < len(bodies); i++ {
		b := bodies[i]
		e += 0.5 * b.mass * (b.vx*b.vx + b.vy*b.vy + b.vz*b.vz)
		for j := i + 1; j < len(bodies); j++ {
			b2 := bodies[j]
			dx := b.x - b2.x
			dy := b.y - b2.y
			dz := b.z - b2.z
			distance := math.Sqrt(dx*dx + dy*dy + dz*dz)
			e -= (b.mass * b2.mass) / distance
		}
	}
	return e
}

func offsetMomentum(bodies []Body) {
	px, py, pz := 0.0, 0.0, 0.0
	for _, b := range bodies {
		px += b.vx * b.mass
		py += b.vy * b.mass
		pz += b.vz * b.mass
	}
	bodies[0].vx = -px / SolarMass
	bodies[0].vy = -py / SolarMass
	bodies[0].vz = -pz / SolarMass
}

func initBodies() []Body {
	return []Body{
		{ // Sun
			x: 0, y: 0, z: 0,
			vx: 0, vy: 0, vz: 0,
			mass: SolarMass,
		},
		{ // Jupiter
			x: 4.84143144246472090e+00, y: -1.16032004402742839e+00, z: -1.03622044471123109e-01,
			vx: 1.66007664274403694e-03 * Days, vy: 7.69901118419740425e-03 * Days, vz: -6.90460016972063023e-05 * Days,
			mass: 9.54791938424326609e-04 * SolarMass,
		},
		{ // Saturn
			x: 8.34336671824457987e+00, y: 4.12479856412430479e+00, z: -4.03523417114321381e-01,
			vx: -2.76742510726862411e-03 * Days, vy: 4.99852801234917238e-03 * Days, vz: 2.30417297573763929e-05 * Days,
			mass: 2.85885980666130812e-04 * SolarMass,
		},
		{ // Uranus
			x: 1.28943695621391310e+01, y: -1.51111514016986312e+01, z: -2.23307578892655734e-01,
			vx: 2.96460137564761618e-03 * Days, vy: 2.37847173959480950e-03 * Days, vz: -2.96589568540237556e-05 * Days,
			mass: 4.36624404335156298e-05 * SolarMass,
		},
		{ // Neptune
			x: 1.53796971148509165e+01, y: -2.59193146099879641e+01, z: 1.79258772950371181e-01,
			vx: 2.68067772490389322e-03 * Days, vy: 1.62824170038242295e-03 * Days, vz: -9.51592254519715870e-05 * Days,
			mass: 5.15138902046611451e-05 * SolarMass,
		},
	}
}

func main() {
	n := 5000000
	bodies := initBodies()
	offsetMomentum(bodies)
	fmt.Printf("%.9f\n", energy(bodies))

	t0 := time.Now()
	for i := 0; i < n; i++ {
		advance(bodies, 0.01)
	}
	t1 := time.Now()

	fmt.Printf("%.9f\n", energy(bodies))
	fmt.Printf("bench_ns=%d\n", t1.Sub(t0).Nanoseconds())
}
