use human_bytes::human_bytes;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

#[global_allocator]
static GLOBAL: pprof_alloc::PprofAlloc = pprof_alloc::PprofAlloc::new()
	.with_pprof_sample_rate_from_env(1)
	.with_stats();

fn main() {
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(4)
		.thread_name_fn(|| {
			static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
			let id = ATOMIC_ID.fetch_add(1, Ordering::SeqCst);
			format!("test-{id}")
		})
		.enable_all()
		.build()
		.unwrap();
	runtime.block_on(async {
		tokio::task::spawn(main_inner()).await.unwrap();
	});
}

async fn main_inner() {
	println!("Starting allocation patterns example...");
	println!(
		"cgroup memory: {:?}",
		pprof_alloc::stats::cgroups::get_memory()
	);
	// 1. Small vector allocations
	println!("1. Allocating small vectors...");
	let _vectors = allocation_helpers::allocate_small_vectors();

	// 2. Large buffer allocation
	println!("2. Allocating large buffer...");
	let _large_buffer = allocation_helpers::allocate_large_buffer();

	// 3. String allocations
	println!("3. Allocating string data...");
	let _string_data = allocation_helpers::allocate_string_data();

	// 4. Nested structure allocations
	println!("4. Allocating nested structures...");
	let _nested = allocation_helpers::allocate_nested_structures();

	// 5. Immediate allocations and frees
	println!("5. Immediate allocate and free pattern...");
	for _ in 0..10 {
		allocation_helpers::allocate_and_immediately_free();
	}

	// 6. Recursive allocations
	println!("6. Recursive allocations...");
	let _recursive_data = recursive_allocations::recursive_allocation(10);
	let _fib_sequence = recursive_allocations::fibonacci_allocation(20);

	// 7. Concurrent allocations
	println!("7. Concurrent allocations...");
	concurrent_allocations::spawn_allocation_threads();

	// 9. Mixed allocation patterns
	println!("9. Mixed allocation patterns...");
	mixed_allocation_patterns();

	// 10. Long-lived allocations
	println!("10. Long-lived allocations...");
	let _long_lived = create_long_lived_allocations();

	// 11. Async allocations
	println!("11. Async allocations...");
	async_allocations().await;

	// 12. Box, Rc, Arc allocations
	println!("12. Smart pointer allocations...");
	smart_pointer_allocations();

	// 13. Nested call stacks
	println!("13. Nested call stacks...");
	nested_call_stacks();

	// 14. Varied size allocations
	println!("14. Varied size allocations...");
	varied_size_allocations();

	// 15. Spawned task allocations
	println!("15. Spawned task allocations...");
	spawned_task_allocations().await;

	std::fs::File::create("/tmp/pprof")
		.unwrap()
		.write(&pprof_alloc::generate_pprof().unwrap())
		.unwrap();
	println!("wrote pprof to /tmp/pprof");

	drop(_large_buffer);
	// pprof_alloc::generate_fragmentation_map().unwrap();
	let by = pprof_alloc::generate_pprof().unwrap();
	fs::write("/tmp/pprof.memprof", by).unwrap();
	println!("Wrote /tmp/pprof.memprof");

	println!("Allocation patterns example completed.");
	println!("Press Enter to exit (keeping some allocations alive)...");
	let m = pprof_alloc::stats::malloc::info().unwrap();
	let s = pprof_alloc::stats::cgroups::get_stats().unwrap();
	let r = pprof_alloc::stats::smaps::rollup().unwrap();
	println!("malloc: {:#?}", m);
	println!(
		"cgroup memory: {:?}",
		pprof_alloc::stats::cgroups::get_memory()
	);
	println!(
		"cgroup stats: {:?}",
		pprof_alloc::stats::cgroups::get_stats()
	);
	println!("smaps memory: {:?}", pprof_alloc::stats::smaps::rollup());

	println!("malloc max:\t\t{}", bytes(m.system_max()));
	println!("malloc cur:\t\t{}", bytes(m.system_current()));
	println!("malloc total:\t\t{}", bytes(m.total()));
	println!("cg usage:\t\t{}", bytes(s.usage));
	println!("cg working_set:\t\t{}", bytes(s.working_set));
	println!("cg anon:\t\t{}", bytes(s.anon));
	println!("cg inactive_anon:\t{}", bytes(s.inactive_anon));
	println!("cg active_anon:\t\t{}", bytes(s.active_anon));
	println!("cg file:\t\t{}", bytes(s.file));
	println!("cg active_file:\t\t{}", bytes(s.active_file));
	println!("cg inactive_file:\t{}", bytes(s.inactive_file));
	println!("cg kernel:\t\t{}", bytes(s.kernel));

	println!("smaps size:\t\t{}", bytes(r.size));
	println!("smaps rss:\t\t{}", bytes(r.rss));
	println!("smaps pss:\t\t{}", bytes(r.pss));
	println!("smaps pss_dirty:\t{}", bytes(r.pss_dirty));
	println!("smaps shared_clean:\t{}", bytes(r.shared_clean));
	println!("smaps shared_dirty:\t{}", bytes(r.shared_dirty));
	println!("smaps private_clean:\t{}", bytes(r.private_clean));
	println!("smaps private_dirty:\t{}", bytes(r.private_dirty));
	println!("smaps referenced:\t{}", bytes(r.referenced));
	println!("smaps anonymous:\t{}", bytes(r.anonymous));
	let _ = std::io::stdin().read_line(&mut String::new());
}

fn bytes(u: u64) -> String {
	human_bytes(u as f64)
}

mod allocation_helpers {
	use std::collections::HashMap;

	pub fn allocate_small_vectors() -> Vec<Vec<u8>> {
		let mut vectors = Vec::new();
		for i in 0..100 {
			let mut vec = Vec::with_capacity(64);
			for j in 0..64 {
				vec.push((i * j) as u8);
			}
			vectors.push(vec);
		}
		vectors
	}

	pub fn allocate_large_buffer() -> Vec<u8> {
		vec![0u8; 1024 * 1024] // 1MB buffer
	}

	pub fn allocate_string_data() -> String {
		let mut result = String::new();
		for i in 0..1000 {
			result.push_str(&format!("Item {}: {}\n", i, i * 2));
		}
		result
	}

	pub fn allocate_nested_structures() -> HashMap<String, Vec<i32>> {
		let mut map = HashMap::new();
		for i in 0..50 {
			let key = format!("key_{}", i);
			let values: Vec<i32> = (0..100).map(|j| i * j).collect();
			map.insert(key, values);
		}
		map
	}

	pub fn allocate_and_immediately_free() {
		let _temp = vec![0u8; 8192];
		let _temp_string = "This will be freed immediately".to_string();
	}
}

mod recursive_allocations {
	use std::collections::VecDeque;

	pub fn recursive_allocation(depth: usize) -> Vec<i32> {
		if depth == 0 {
			return vec![1, 2, 3, 4, 5];
		}

		let mut result = recursive_allocation(depth - 1);
		result.extend_from_slice(&[depth as i32; 10]);
		result
	}

	pub fn fibonacci_allocation(n: usize) -> VecDeque<usize> {
		let mut sequence = VecDeque::new();
		let mut a = 0;
		let mut b = 1;

		for _ in 0..n {
			sequence.push_back(a);
			let temp = a + b;
			a = b;
			b = temp;
		}
		sequence
	}
}

mod concurrent_allocations {
	use std::sync::{Arc, Mutex};
	use std::thread;
	use std::time::Duration;

	pub fn spawn_allocation_threads() {
		let counter = Arc::new(Mutex::new(0));
		let mut handles = Vec::new();

		for i in 0..4 {
			let counter_clone = Arc::clone(&counter);
			let handle = thread::spawn(move || {
				worker_thread(i, counter_clone);
			});
			handles.push(handle);
		}

		for handle in handles {
			handle.join().unwrap();
		}
	}

	fn worker_thread(id: usize, counter: Arc<Mutex<usize>>) {
		for round in 0..10 {
			// Allocate different sizes based on thread ID and round
			let size = 1024 * (id + 1) * (round + 1);
			let _data = vec![0u8; size];

			// Update shared counter
			{
				let mut count = counter.lock().unwrap();
				*count += 1;
			}

			thread::sleep(Duration::from_millis(10));
		}
	}
}

fn mixed_allocation_patterns() {
	// Mix of different allocation types in the same call stack
	let mut string_map = std::collections::HashMap::new();

	for i in 0..50 {
		let key = format!("mixed_key_{}", i);
		let value = format!("value_with_some_data_{}", i);
		string_map.insert(key, value);

		// Allocate some binary data
		let _binary_data = vec![i as u8; 256];

		// Allocate a small struct
		let small_struct = SmallStruct {
			id: i,
			data: vec![i; 32],
		};
		let _ = small_struct.id + small_struct.data.len();
	}
}

fn create_long_lived_allocations() -> Vec<LongLivedData> {
	let mut data = Vec::new();

	for i in 0..10 {
		let item = LongLivedData {
			id: i,
			buffer: vec![0u8; 1024 * 100], // 10KB each
			metadata: format!("Metadata for item {}", i),
			counters: vec![0; 100],
		};
		let _ = item.id + item.buffer.len() + item.metadata.len() + item.counters.len();
		data.push(item);
	}

	data
}

async fn async_allocations() {
	// Allocate in async context
	let _async_vec = vec![0u8; 2048];

	// Spawn a task that allocates
	let handle = tokio::spawn(async {
		let _task_vec = vec![1u8; 1024];
		std::thread::sleep(std::time::Duration::from_millis(50));
	});

	// Another allocation
	let _another_vec = vec![2u8; 512];

	handle.await.unwrap();
}

fn smart_pointer_allocations() {
	use std::rc::Rc;
	use std::sync::Arc;

	// Box allocations
	let _boxed_int = Box::new(42);
	let _boxed_vec = Box::new(vec![0u8; 256]);

	// Rc allocations
	let _rc_vec = Rc::new(vec![1u8; 128]);
	let _rc_string = Rc::new("Rc string".to_string());

	// Arc allocations
	let _arc_vec = Arc::new(vec![2u8; 64]);
	let _arc_hashmap = Arc::new(std::collections::HashMap::<String, Vec<u8>>::new());
}

fn nested_call_stacks() {
	level1();
}

fn level1() {
	level2();
	let _alloc1 = vec![0u8; 100];
	let _ = _alloc1.capacity();
}

fn level2() {
	level3();
	let _alloc2 = vec![1u8; 200];
	let _ = _alloc2.capacity();
}

fn level3() {
	level4();
	let _alloc3 = vec![2u8; 300];
}

fn level4() {
	let _alloc4 = vec![3u8; 400];
}

fn varied_size_allocations() {
	// Powers of 2
	for i in 0..10 {
		let size = 1 << i; // 1, 2, 4, ..., 512
		let _vec = vec![0u8; size];
	}

	// Random sizes
	use rand::Rng;
	let mut rng = rand::thread_rng();
	for _ in 0..20 {
		let size = rng.gen_range(1..=1000);
		let _vec = vec![0u8; size];
	}

	// Large allocations
	let _large1 = vec![0u8; 10000];
	let _large2 = vec![0u8; 50000];
}

async fn spawned_task_allocations() {
	let mut handles = Vec::new();

	for i in 0..5 {
		let handle = tokio::spawn(async move {
			task_allocation_pattern(i).await;
		});
		handles.push(handle);
	}

	for handle in handles {
		handle.await.unwrap();
	}
}

async fn task_allocation_pattern(id: usize) {
	// Different patterns per task
	match id % 3 {
		0 => {
			let _vec = vec![id as u8; 1000];
			std::thread::sleep(std::time::Duration::from_millis(10));
		},
		1 => {
			let mut map = std::collections::HashMap::new();
			for j in 0..10 {
				map.insert(format!("key_{}_{}", id, j), vec![j as u8; 50]);
			}
		},
		2 => {
			let _string = (0..100).map(|_| "word ").collect::<String>();
		},
		_ => {},
	}
}

#[derive(Debug)]
struct SmallStruct {
	id: usize,
	data: Vec<usize>,
}

#[derive(Debug)]
struct LongLivedData {
	id: usize,
	buffer: Vec<u8>,
	metadata: String,
	counters: Vec<usize>,
}
