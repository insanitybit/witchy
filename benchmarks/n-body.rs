use std::f64::consts::PI;
use std::time::Instant;

const SOLAR_MASS: f64 = 4.0 * PI * PI;
const DAYS: f64 = 365.24;

#[derive(Clone, Copy)]
struct Body {
    x: f64, y: f64, z: f64,
    vx: f64, vy: f64, vz: f64,
    mass: f64,
}

fn advance(bodies: &mut [Body; 5], dt: f64) {
    for i in 0..bodies.len() {
        let (left, right) = bodies.split_at_mut(i + 1);
        let b = &mut left[i];
        for b2 in right.iter_mut() {
            let dx = b.x - b2.x;
            let dy = b.y - b2.y;
            let dz = b.z - b2.z;
            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            let mag = dt / (distance * distance * distance);

            b.vx -= dx * b2.mass * mag;
            b.vy -= dy * b2.mass * mag;
            b.vz -= dz * b2.mass * mag;

            b2.vx += dx * b.mass * mag;
            b2.vy += dy * b.mass * mag;
            b2.vz += dz * b.mass * mag;
        }
    }

    for b in bodies.iter_mut() {
        b.x += dt * b.vx;
        b.y += dt * b.vy;
        b.z += dt * b.vz;
    }
}

fn energy(bodies: &[Body; 5]) -> f64 {
    let mut e = 0.0;
    for i in 0..bodies.len() {
        let b = &bodies[i];
        e += 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for j in (i + 1)..bodies.len() {
            let b2 = &bodies[j];
            let dx = b.x - b2.x;
            let dy = b.y - b2.y;
            let dz = b.z - b2.z;
            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            e -= (b.mass * b2.mass) / distance;
        }
    }
    e
}

fn offset_momentum(bodies: &mut [Body; 5]) {
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for b in bodies.iter() {
        px += b.vx * b.mass;
        py += b.vy * b.mass;
        pz += b.vz * b.mass;
    }
    bodies[0].vx = -px / SOLAR_MASS;
    bodies[0].vy = -py / SOLAR_MASS;
    bodies[0].vz = -pz / SOLAR_MASS;
}

fn init_bodies() -> [Body; 5] {
    [
        Body { // Sun
            x: 0.0, y: 0.0, z: 0.0,
            vx: 0.0, vy: 0.0, vz: 0.0,
            mass: SOLAR_MASS,
        },
        Body { // Jupiter
            x: 4.84143144246472090e+00, y: -1.16032004402742839e+00, z: -1.03622044471123109e-01,
            vx: 1.66007664274403694e-03 * DAYS, vy: 7.69901118419740425e-03 * DAYS, vz: -6.90460016972063023e-05 * DAYS,
            mass: 9.54791938424326609e-04 * SOLAR_MASS,
        },
        Body { // Saturn
            x: 8.34336671824457987e+00, y: 4.12479856412430479e+00, z: -4.03523417114321381e-01,
            vx: -2.76742510726862411e-03 * DAYS, vy: 4.99852801234917238e-03 * DAYS, vz: 2.30417297573763929e-05 * DAYS,
            mass: 2.85885980666130812e-04 * SOLAR_MASS,
        },
        Body { // Uranus
            x: 1.28943695621391310e+01, y: -1.51111514016986312e+01, z: -2.23307578892655734e-01,
            vx: 2.96460137564761618e-03 * DAYS, vy: 2.37847173959480950e-03 * DAYS, vz: -2.96589568540237556e-05 * DAYS,
            mass: 4.36624404335156298e-05 * SOLAR_MASS,
        },
        Body { // Neptune
            x: 1.53796971148509165e+01, y: -2.59193146099879641e+01, z: 1.79258772950371181e-01,
            vx: 2.68067772490389322e-03 * DAYS, vy: 1.62824170038242295e-03 * DAYS, vz: -9.51592254519715870e-05 * DAYS,
            mass: 5.15138902046611451e-05 * SOLAR_MASS,
        }
    ]
}

fn main() {
    let n = 5000000;
    let mut bodies = init_bodies();
    offset_momentum(&mut bodies);
    
    println!("{:.9}", energy(&bodies));

    let t0 = Instant::now();
    for _ in 0..n {
        advance(&mut bodies, 0.01);
    }
    let t1 = Instant::now();

    println!("{:.9}", energy(&bodies));
    println!("bench_ns={}", t1.duration_since(t0).as_nanos());
}
