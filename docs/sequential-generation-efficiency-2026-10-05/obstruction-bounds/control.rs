use reda::compile::geometry::Anchor;
fn reference_batch(
    chain: impl IntoIterator<Item = Anchor>,
    at: Anchor,
    candidates: &[Anchor],
    seed_rules: bool,
    obstructed: &mut [bool],
) {
    debug_assert_eq!(candidates.len(), obstructed.len());
    obstructed.fill(false);
    let mut successor: Option<Anchor> = None;
    for cell in chain {
        let rises_after = successor.is_some_and(|after: Anchor| after.y > cell.y);
        for (candidate, obstructed) in candidates.iter().zip(obstructed.iter_mut()) {
            if *obstructed || cell.x != candidate.x || cell.z != candidate.z {
                continue;
            }
            if cell.y == candidate.y
                || (candidate.y < at.y && cell.y == at.y + 1)
                || (seed_rules && cell.y == candidate.y + 1)
                || cell.y == candidate.y - 1
                || (cell.y == candidate.y - 2 && rises_after)
            {
                *obstructed = true;
            }
        }
        successor = Some(cell);
    }
}

fn self_obstructs_batch(
    chain: impl IntoIterator<Item = Anchor>,
    at: Anchor,
    candidates: &[Anchor],
    seed_rules: bool,
    obstructed: &mut [bool],
) {
    debug_assert_eq!(candidates.len(), obstructed.len());
    obstructed.fill(false);
    let Some(first) = candidates.first() else { return; };
    let (mut min_x, mut max_x, mut min_z, mut max_z) = (first.x, first.x, first.z, first.z);
    for candidate in &candidates[1..] {
        min_x = min_x.min(candidate.x); max_x = max_x.max(candidate.x);
        min_z = min_z.min(candidate.z); max_z = max_z.max(candidate.z);
    }
    let mut successor: Option<Anchor> = None;
    for cell in chain {
        let rises_after = successor.is_some_and(|after: Anchor| after.y > cell.y);
        // Every obstruction predicate requires the same x/z column.
        // Preserve the successor even when this cell cannot match a candidate.
        successor = Some(cell);
        if cell.x < min_x || cell.x > max_x || cell.z < min_z || cell.z > max_z {
            continue;
        }
        for (candidate, obstructed) in candidates.iter().zip(obstructed.iter_mut()) {
            if *obstructed || cell.x != candidate.x || cell.z != candidate.z {
                continue;
            }
            if cell.y == candidate.y
                || (candidate.y < at.y && cell.y == at.y + 1)
                || (seed_rules && cell.y == candidate.y + 1)
                || cell.y == candidate.y - 1
                || (cell.y == candidate.y - 2 && rises_after)
            {
                *obstructed = true;
            }
        }
    }
}
fn main(){
 let at=Anchor{x:0,y:3,z:0};let mut checks=0;
 for len in [0,1,3,12,32,100,300] {for seed in [false,true]{for offset in -4..5 {
  let chain=(0..len).map(|i|Anchor{x:i/5-20,y:i%8,z:(i*7)%17-8}).collect::<Vec<_>>();
  let candidates=(-3..4).map(|i|Anchor{x:i+offset,y:i%3+3,z:i%2}).collect::<Vec<_>>();
  let mut a=vec![true;candidates.len()];let mut b=a.clone();reference_batch(chain.iter().copied(),at,&candidates,seed,&mut a);self_obstructs_batch(chain.iter().copied(),at,&candidates,seed,&mut b);assert_eq!(a,b);checks+=1;
 }}}
 // Outside-column successors still affect a following matching rising cell.
 let chain=[Anchor{x:90,y:5,z:90},Anchor{x:0,y:1,z:0}];let candidates=[Anchor{x:0,y:3,z:0}];let mut a=[false];let mut b=[false];reference_batch(chain,at,&candidates,true,&mut a);self_obstructs_batch(chain,at,&candidates,true,&mut b);assert_eq!(a,b);assert!(a[0]);
 let mut empty=[];self_obstructs_batch(chain,at,&[],true,&mut empty);
 let chain=(0..150).map(|i|Anchor{x:i,y:3,z:0}).collect::<Vec<_>>();let candidates=(-1..2).flat_map(|x|(-1..2).map(move|y|Anchor{x,y:y+3,z:0})).collect::<Vec<_>>();
 let mut bits=vec![false;candidates.len()];let repetitions=100000;let t=std::time::Instant::now();for _ in 0..repetitions{reference_batch(std::hint::black_box(&chain).iter().copied(),at,std::hint::black_box(&candidates),true,&mut bits);std::hint::black_box(&bits);}let old=t.elapsed();let t=std::time::Instant::now();for _ in 0..repetitions{self_obstructs_batch(std::hint::black_box(&chain).iter().copied(),at,std::hint::black_box(&candidates),true,&mut bits);std::hint::black_box(&bits);}let new=t.elapsed();println!("{checks} comparisons plus empty/rising-successor controls pass; synthetic long-prefix reference={old:?}, bounds={new:?}; not full generator timing.");
}
