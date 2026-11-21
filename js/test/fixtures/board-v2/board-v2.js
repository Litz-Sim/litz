"use components";
function __jcoEmbeddedRuntime$promiseWithResolvers() {
	if (Promise.withResolvers) return Promise.withResolvers();
	let resolve;
	let reject;
	return {
		promise: new Promise((res, rej) => {
			resolve = res;
			reject = rej;
		}),
		resolve,
		reject
	};
}
function __jcoEmbeddedRuntime$isPromiseLike(value) {
	return value !== null && (typeof value == "object" || typeof value == "function") && typeof value.then == "function";
}
const __jcoEmbeddedRuntime$debugLog = (...args) => {
	if (!globalThis?.process?.env?.JCO_DEBUG) return;
	console.debug(...args);
};
const __jcoEmbeddedRuntime$initializeTaskRegisters = () => ({});
function __jcoEmbeddedRuntime$validate(args) {
	if (!args) throw TypeError("args missing");
	if (args.taskID === void 0) throw TypeError("missing task ID");
	if (args.threadID === void 0) throw TypeError("missing thread ID");
	if (args.componentIdx === void 0) throw TypeError("missing component idx");
}
const __jcoEmbeddedRuntime$getCurrentTaskMeta = (registers, componentIdx) => {
	let value = componentIdx == null ? registers.current : registers[componentIdx];
	return value == null ? void 0 : { ...value };
};
const __jcoEmbeddedRuntime$setCurrentTaskMeta = (registers, args) => {
	__jcoEmbeddedRuntime$validate(args);
	let { taskID, threadID, componentIdx } = args;
	return registers.current = registers[componentIdx] = {
		taskID,
		threadID,
		componentIdx
	};
};
function __jcoEmbeddedRuntime$withCurrentTaskMeta(registers, args) {
	__jcoEmbeddedRuntime$debugLog("[_withGlobalCurrentThreadMeta()] args", args);
	__jcoEmbeddedRuntime$validate(args);
	if (!args.fn) throw TypeError("missing fn");
	let { taskID, threadID, componentIdx, fn } = args;
	let previous = registers[componentIdx] ?? null;
	let previousCurrent = registers.current ?? null;
	try {
		registers.current = registers[componentIdx] = {
			taskID,
			threadID,
			componentIdx
		};
		return fn();
	} catch (error) {
		__jcoEmbeddedRuntime$debugLog("error while executing sync callee/callback", {
			...args,
			err: error
		});
		throw error;
	} finally {
		registers[componentIdx] = previous;
		registers.current = previousCurrent;
	}
}
function __jcoEmbeddedRuntime$withCurrentTaskMetaAsync(registers, args) {
	__jcoEmbeddedRuntime$debugLog("[_withGlobalCurrentThreadMetaAsync()] args", args);
	__jcoEmbeddedRuntime$validate(args);
	if (!args.fn) throw TypeError("missing fn");
	let { taskID, threadID, componentIdx, fn } = args;
	let previous = registers[componentIdx] ?? null;
	let previousCurrent = registers.current ?? null;
	let result;
	try {
		registers.current = registers[componentIdx] = {
			taskID,
			threadID,
			componentIdx
		};
		result = fn();
	} catch (error) {
		__jcoEmbeddedRuntime$debugLog("error while executing sync callee/callback", {
			...args,
			err: error
		});
		registers[componentIdx] = previous;
		registers.current = previousCurrent;
		throw error;
	}
	if (!__jcoEmbeddedRuntime$isPromiseLike(result)) {
		registers[componentIdx] = previous;
		registers.current = previousCurrent;
		return result;
	}
	return (async () => {
		try {
			return await result;
		} catch (error) {
			__jcoEmbeddedRuntime$debugLog("error while executing async callee/callback", {
				...args,
				err: error
			});
			throw error;
		} finally {
			registers[componentIdx]?.threadID === threadID && registers[componentIdx]?.taskID === taskID && (registers[componentIdx] = null);
			registers.current?.threadID === threadID && registers.current?.taskID === taskID && (registers.current = null);
		}
	})();
}
const __jcoEmbeddedRuntime$clearCurrentTaskMeta = async (registers, args) => {
	__jcoEmbeddedRuntime$debugLog("[_clearCurrentTask()] args", args);
	__jcoEmbeddedRuntime$validate(args);
	let { taskID, threadID, componentIdx } = args;
	let meta = registers[componentIdx];
	if (!meta) throw Error(`missing current thread meta for component idx [${componentIdx}]`);
	if (meta.threadID !== threadID) throw Error(`thread ID [${meta.threadID}] != requested ID [${threadID}]`);
	if (meta.taskID !== taskID) throw Error(`task ID [${meta.taskID}] != requested ID [${taskID}]`);
	if (meta.componentIdx !== componentIdx) throw Error(`component idx [${meta.componentIdx}] != requested idx [${componentIdx}]`);
	registers[componentIdx] = null;
	registers.current?.threadID === threadID && registers.current?.taskID === taskID && (registers.current = null);
};
const __jcoEmbeddedRuntime$reportTrap = (context, err) => {
	let { storeTrap, RuntimeError } = context;
	if (storeTrap.onTrap === null || storeTrap.stopped) return;
	if (!(err instanceof RuntimeError) || storeTrap.trapsReported.has(err)) return;
	storeTrap.trapsReported.add(err);
	let trap = __jcoEmbeddedRuntime$normalizeCoreTrap(context.normalization, err);
	try {
		storeTrap.onTrap(trap);
	} catch (e) {
		queueMicrotask(() => {
			throw e;
		});
	}
};
function __jcoEmbeddedRuntime$isIdle(context) {
	if (context.storeTrap.error !== null) return !0;
	for (let [componentIdx, metas] of context.tasks) if (componentIdx !== -1 && metas.length > 0) return !1;
	return !0;
}
const __jcoEmbeddedRuntime$checkIdle = (context) => {
	context.checkCalls?.();
	let { idleWaiters } = context.storeAsyncState;
	if (idleWaiters.size === 0 || !__jcoEmbeddedRuntime$isIdle(context)) return;
	let waiters = [...idleWaiters];
	idleWaiters.clear();
	for (let resolve of waiters) resolve();
};
const __jcoEmbeddedRuntime$whenIdle = (context) => {
	if (__jcoEmbeddedRuntime$isIdle(context)) return Promise.resolve();
	return new Promise((resolve) => {
		context.storeAsyncState.idleWaiters.add(resolve);
	});
};
const __jcoEmbeddedRuntime$stopInstance = (context) => {
	let { storeTrap, storeAsyncState, asyncState, symbolDispose } = context;
	if (storeTrap.stopped) return;
	storeTrap.stopped = !0;
	let err = new context.RuntimeError("component instance has been stopped");
	storeTrap.error ??= err;
	clearTimeout(storeAsyncState.deadlockCheck);
	storeAsyncState.deadlockCheck = null;
	for (let state of asyncState.values()) state.markTrapped(err);
	for (let reject of storeTrap.stopWaiters) reject(err);
	storeTrap.stopWaiters.clear();
	let pending = [...context.tasks.values()].flat().map((meta) => meta.task);
	for (let task of pending) task.stop(err);
	for (let state of asyncState.values()) state.stop();
	let errors = [];
	for (let channels of context.channels) for (let i = 2, data = channels.data(); i < data.length; i += 2) {
		let channel = data[i];
		if (channel === void 0 || channel === context.RepTable.FREE) continue;
		try {
			channel.onInstanceStop?.();
		} catch (e) {
			errors.push(e);
		}
	}
	let resources = new Set();
	for (let { handles, captures } of storeTrap.resourceTables) for (let i = 2; i < handles.length; i += 2) {
		if (handles[i] & 1073741824 || !(handles[i + 1] >= __jcoEmbeddedRuntime$tableOwn)) continue;
		let resource = captures.get(handles[i + 1] - __jcoEmbeddedRuntime$tableOwn);
		resource !== void 0 && resources.add(resource);
	}
	for (let { captures } of storeTrap.resourceTables) captures.clear();
	storeTrap.resourceTables.length = 0;
	for (let resource of resources) {
		if (storeTrap.disposed.has(resource)) continue;
		try {
			resource[symbolDispose]?.();
		} catch (error) {
			errors.push(error);
		}
	}
	if (errors.length) throw AggregateError(errors, "failed to dispose stopped instance resources");
};
const __jcoEmbeddedRuntime$initializeInstanceFlags = () => new Map();
const __jcoEmbeddedRuntime$initializeStoreTrap = (onTrap = null) => ({
	error: null,
	stopped: !1,
	resourceTables: [],
	stopWaiters: new Set(),
	disposed: new WeakSet(),
	onTrap,
	trapsReported: new WeakSet()
});
const __jcoEmbeddedRuntime$initializeStoreAsyncState = () => ({
	deadlockCheck: null,
	pendingHostOperations: 0,
	idleWaiters: new Set()
});
const __jcoEmbeddedRuntime$initializeComponentAsyncStates = () => new Map();
const __jcoEmbeddedRuntime$initializeCoreTrapMessages = (messages) => new Map(Object.entries(messages));
function __jcoEmbeddedRuntime$normalizeCoreTrap(context, err) {
	let { RuntimeError, messages: trapMessages } = context;
	if (!(err instanceof RuntimeError)) return err;
	let message = trapMessages.get(err.message);
	message !== void 0 && (err.message = message);
	return err;
}
function __jcoEmbeddedRuntime$checkForDeadlock(context) {
	let { RuntimeError, storeAsyncState, storeTrap, asyncState } = context;
	if (storeAsyncState.deadlockCheck !== null || storeTrap.error !== null) return;
	storeAsyncState.deadlockCheck = setTimeout(() => {
		storeAsyncState.deadlockCheck = null;
		if (storeTrap.error !== null || storeAsyncState.pendingHostOperations > 0) return;
		let suspendedTasks = new Set();
		for (let state of asyncState.values()) {
			if (state.hasPendingSchedulerWork()) {
				state.runTickLoop();
				return;
			}
			for (let meta of state.suspendedTaskMetas()) suspendedTasks.add(meta.task);
		}
		let unresolvedRoots = new Set();
		for (let task of suspendedTasks) {
			let root = task.getRootTask();
			root.isResolvedState() || unresolvedRoots.add(root);
		}
		if (unresolvedRoots.size === 0) return;
		let err = new RuntimeError("wasm trap: deadlock detected: event loop cannot make further progress");
		err.deadlockDetail = {
			pendingHostOperations: storeAsyncState.pendingHostOperations,
			suspendedTasks: [...suspendedTasks].map((task) => ({
				taskID: task.id(),
				componentIdx: task.componentIdx(),
				state: task.taskState(),
				rootTaskID: task.getRootTask().id()
			})),
			unresolvedRootTaskIDs: [...unresolvedRoots].map((root) => root.id())
		};
		storeTrap.error = err;
		for (let root of unresolvedRoots) {
			root.setErrored(err);
			root.reject(err);
		}
		for (let task of suspendedTasks) if (!task.isResolvedState() && unresolvedRoots.has(task.getRootTask())) {
			task.setErrored(err);
			task.reject(err);
		}
		for (let state of asyncState.values()) state.markTrapped(err);
		for (let state of asyncState.values()) state.abandonLockWaiters();
		for (let state of asyncState.values()) state.abandonAdmissionWaiters();
		for (let state of asyncState.values()) state.runTickLoop();
	}, 0);
}
function __jcoEmbeddedRuntime$trackHostOperation(context, operation) {
	let { storeAsyncState, asyncState, symbolDispose, storeTrap } = context;
	if (storeTrap.stopped) throw storeTrap.error;
	let result = operation();
	if (!__jcoEmbeddedRuntime$isPromiseLike(result)) {
		if (storeTrap.stopped) throw storeTrap.error;
		return result;
	}
	storeAsyncState.pendingHostOperations++;
	let rejectStopped;
	let stopped = new Promise((_, reject) => {
		rejectStopped = reject;
	});
	storeTrap.stopWaiters.add(rejectStopped);
	storeTrap.stopped && rejectStopped(storeTrap.error);
	let tracked = Promise.race([result, stopped]).finally(() => {
		storeTrap.stopWaiters.delete(rejectStopped);
		storeAsyncState.pendingHostOperations--;
		if (storeAsyncState.pendingHostOperations < 0) throw Error("negative pending host operation count");
		for (let state of asyncState.values()) state.runTickLoop();
		__jcoEmbeddedRuntime$checkForDeadlock(context);
	});
	for (let sym of [Symbol.asyncDispose, symbolDispose]) typeof sym == "symbol" && typeof result[sym] == "function" && (tracked[sym] = () => result[sym]());
	return tracked;
}
function __jcoEmbeddedRuntime$checkMayLeave(context, componentIdx) {
	let { RuntimeError, instanceFlags } = context;
	if (instanceFlags.get(componentIdx)?.value !== 1) throw new RuntimeError("cannot leave component instance");
}
function __jcoEmbeddedRuntime$recordTrap(context, componentIdx, err, convertsRangeErrors = !1) {
	let { RuntimeError, registers: currentTaskMeta } = context;
	convertsRangeErrors && err instanceof RangeError && (err = new RuntimeError("out of bounds memory access"));
	if (err instanceof RuntimeError) {
		let idx = componentIdx ?? currentTaskMeta.current?.componentIdx;
		idx != null && __jcoEmbeddedRuntime$getOrCreateAsyncState(context.lookup, idx).markTrapped(err);
	}
	return err;
}
function __jcoEmbeddedRuntime$guardTrap(context, componentIdx, fn, convertsRangeErrors = !1) {
	return function(...args) {
		try {
			return fn.apply(this, args);
		} catch (err) {
			throw __jcoEmbeddedRuntime$recordTrap(context, componentIdx, err, convertsRangeErrors);
		}
	};
}
function __jcoEmbeddedRuntime$getOrCreateAsyncState(context, componentIdx) {
	let { asyncState, ComponentAsyncState } = context;
	if (!asyncState.has(componentIdx)) {
		let newState = new ComponentAsyncState({ componentIdx });
		asyncState.set(componentIdx, newState);
	}
	return asyncState.get(componentIdx);
}
const __jcoEmbeddedRuntime$TICK_DONE = "done";
const __jcoEmbeddedRuntime$TICK_RESUMED = "resumed";
const __jcoEmbeddedRuntime$TICK_IDLE = "idle";
function __jcoEmbeddedRuntime$initializeComponentAsyncStateClass(context) {
	let { RuntimeError, RepTable, indexSpaces, instanceFlags, storeTrap, normalization, cannotEnter, deadlock, Waitable } = context;
	class ComponentAsyncState {
		static EVENT_HANDLER_EVENTS = ["backpressure-change"];
		static TickResult = {
			DONE: __jcoEmbeddedRuntime$TICK_DONE,
			RESUMED: __jcoEmbeddedRuntime$TICK_RESUMED,
			IDLE: __jcoEmbeddedRuntime$TICK_IDLE
		};
		#componentIdx;
		#callingAsyncImport = !1;
		#syncImportWait = __jcoEmbeddedRuntime$promiseWithResolvers();
		#lockHolderTaskID = null;
		#lockWaiters = [];
		#admissionQueue = [];
		#lockHandoffScheduled = !1;
		#pendingTaskStarts = 0;
		#suspendedThreadsByRef = new Map();
		#errored = null;
		#trapped = !1;
		#trapError = null;
		#backpressure = 0;
		#backpressureWaiters = 0n;
		#handlers = [];
		#nextHandlerID = 0n;
		#tickLoop = null;
		#onExclusiveReleaseHandlers = [];
		#mayLeave = !0;
		handles;
		subtasks;
		threads;
		constructor(args) {
			this.#componentIdx = args.componentIdx;
			this.handles = new RepTable({
				target: `component [${this.#componentIdx}] handles (waitable objects)`,
				space: __jcoEmbeddedRuntime$getHandleIndexSpace(indexSpaces, this.#componentIdx)
			});
			this.subtasks = new RepTable({ target: `component [${this.#componentIdx}] subtasks` });
			this.threads = new RepTable({ target: `component [${this.#componentIdx}] threads` });
		}
		componentIdx() {
			return this.#componentIdx;
		}
		get mayLeave() {
			let flags = instanceFlags.get(this.#componentIdx);
			return flags === void 0 ? this.#mayLeave : flags.value === 1;
		}
		set mayLeave(value) {
			if (typeof value != "boolean") throw TypeError("mayLeave must be a boolean");
			this.#mayLeave = value;
			let flags = instanceFlags.get(this.#componentIdx);
			flags !== void 0 && (flags.value = +!!value);
		}
		errored() {
			return this.#errored !== null;
		}
		setErrored(err) {
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#setErrored()] component errored", {
				err,
				componentIdx: this.#componentIdx
			});
			if (this.#errored) return;
			err ||= Object.assign(Error("error elswehere (see other component instance error)"), { componentIdx: this.#componentIdx });
			this.#errored = err;
		}
		markTrapped(err) {
			if (!(err instanceof RuntimeError)) return !1;
			let trap = __jcoEmbeddedRuntime$normalizeCoreTrap(normalization, err);
			this.#trapped = !0;
			this.#trapError === null && (this.#trapError = trap);
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#markTrapped()] component trapped", {
				err,
				componentIdx: this.#componentIdx
			});
			storeTrap.error === null && (storeTrap.error = trap);
			context.reportTrap?.(trap);
			context.checkIdle?.();
			return !0;
		}
		isTrapped() {
			return this.#trapped;
		}
		throwIfInterrupted() {
			if (storeTrap.stopped) throw storeTrap.error;
			if (this.#trapped) throw this.#trapError ?? new RuntimeError(cannotEnter);
		}
		throwIfTrapped() {
			if (storeTrap.stopped) throw storeTrap.error;
			if (this.#trapped) throw new RuntimeError(cannotEnter);
		}
		callingSyncImport(val) {
			if (val === void 0) return this.#callingAsyncImport;
			if (typeof val != "boolean") throw TypeError("invalid setting for async import");
			let prev = this.#callingAsyncImport;
			this.#callingAsyncImport = val;
			prev === !0 && this.#callingAsyncImport === !1 && this.#notifySyncImportEnd();
		}
		#notifySyncImportEnd() {
			let existing = this.#syncImportWait;
			this.#syncImportWait = __jcoEmbeddedRuntime$promiseWithResolvers();
			existing.resolve();
		}
		async waitForSyncImportCallEnd() {
			await this.#syncImportWait.promise;
		}
		setBackpressure(v) {
			this.#backpressure = v;
			for (let handler of this.#handlers.filter((handler) => handler.event === "backpressure-change")) handler.fn(v);
			return this.#backpressure;
		}
		getBackpressure() {
			return this.#backpressure;
		}
		incrementBackpressure() {
			let current = this.#backpressure;
			if (current < 0 || current > 2 ** 16) throw Error(`invalid current backpressure value [${current}]`);
			let newValue = this.getBackpressure() + 1;
			if (newValue >= 2 ** 16) throw Error(`invalid new backpressure value [${newValue}], overflow`);
			return this.setBackpressure(newValue);
		}
		decrementBackpressure() {
			let current = this.#backpressure;
			if (current < 0 || current > 2 ** 16) throw Error(`invalid current backpressure value [${current}]`);
			let newValue = current - 1;
			if (newValue < 0) throw new RuntimeError("backpressure counter underflow");
			return this.setBackpressure(newValue);
		}
		hasBackpressure() {
			return this.#backpressure > 0;
		}
		waitForBackpressure() {
			if (!this.hasBackpressure()) return Promise.resolve(null);
			this.addBackpressureWaiter();
			return new Promise((resolve) => {
				let pending = !0;
				let handlerID = this.registerHandler({
					event: "backpressure-change",
					fn: (bp) => {
						if (bp !== 0 || !pending) return;
						pending = !1;
						this.removeHandler({
							event: "backpressure-change",
							handlerID
						});
						this.removeBackpressureWaiter();
						resolve(null);
					}
				});
			});
		}
		registerHandler(args) {
			let { event, fn } = args;
			if (!event) throw Error("missing handler event");
			if (!fn) throw Error("missing handler fn");
			if (!ComponentAsyncState.EVENT_HANDLER_EVENTS.includes(event)) throw Error(`unrecognized event handler [${event}]`);
			let handlerID = this.#nextHandlerID++;
			this.#handlers.push({
				id: handlerID,
				fn,
				event
			});
			return handlerID;
		}
		removeHandler(args) {
			let { event, handlerID } = args;
			this.#handlers = this.#handlers.filter((handler) => handler.event !== event || handler.id !== handlerID);
		}
		getBackpressureWaiters() {
			return this.#backpressureWaiters;
		}
		addBackpressureWaiter() {
			this.#backpressureWaiters++;
		}
		removeBackpressureWaiter() {
			this.#backpressureWaiters--;
			if (this.#backpressureWaiters < 0) throw Error("unexepctedly negative number of backpressure waiters");
		}
		isExclusivelyLocked() {
			return this.#lockHolderTaskID !== null;
		}
		exclusivelyLockedBy(taskID) {
			return this.#lockHolderTaskID === taskID;
		}
		exclusiveLock(taskID) {
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#exclusiveLock()]", {
				holder: this.#lockHolderTaskID,
				requester: taskID,
				componentIdx: this.#componentIdx
			});
			if (taskID == null) throw Error("exclusive lock requires the acquiring task id");
			if (this.#lockHolderTaskID !== null) throw Error(`component [${this.#componentIdx}] exclusive lock held by task [${this.#lockHolderTaskID}], requested by [${taskID}]`);
			this.#lockHolderTaskID = taskID;
		}
		acquireExclusiveLock(taskID) {
			if (taskID == null) throw Error("exclusive lock requires the acquiring task id");
			if (this.#lockHolderTaskID === null) {
				this.#lockHolderTaskID = taskID;
				__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#acquireExclusiveLock()] acquired", {
					holder: taskID,
					componentIdx: this.#componentIdx
				});
				return;
			}
			if (this.#lockHolderTaskID === taskID) throw Error(`task [${taskID}] already holds the lock for component [${this.#componentIdx}]`);
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#acquireExclusiveLock()] waiting", {
				holder: this.#lockHolderTaskID,
				requester: taskID,
				componentIdx: this.#componentIdx,
				queued: this.#lockWaiters.length
			});
			return new Promise((resolve) => {
				this.#lockWaiters.push({
					taskID,
					resolve
				});
			});
		}
		takeAdmissionTicket(taskID) {
			let ticket = {
				taskID,
				...__jcoEmbeddedRuntime$promiseWithResolvers()
			};
			this.#admissionQueue.push(ticket);
			return ticket;
		}
		hasPendingAdmissions() {
			return this.#admissionQueue.length > 0;
		}
		waitForAdmissionTurn(ticket) {
			if (this.#admissionQueue[0] === ticket) return;
			return ticket.promise;
		}
		releaseAdmissionTicket(ticket) {
			let idx = this.#admissionQueue.indexOf(ticket);
			if (idx === -1) return;
			this.#admissionQueue.splice(idx, 1);
			idx === 0 && this.#admissionQueue.length > 0 && this.#admissionQueue[0].resolve();
		}
		abandonAdmissionWaiters() {
			let tickets = this.#admissionQueue.splice(0);
			for (let ticket of tickets) ticket.resolve();
		}
		abandonLockWaiters() {
			let waiters = this.#lockWaiters.splice(0);
			for (let waiter of waiters) waiter.resolve();
		}
		cancelExclusiveLockWaiter(taskID) {
			let idx = this.#lockWaiters.findIndex((waiter) => waiter.taskID === taskID);
			if (idx === -1) return !1;
			let [waiter] = this.#lockWaiters.splice(idx, 1);
			waiter.resolve();
			return !0;
		}
		exclusiveRelease(taskID) {
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#exclusiveRelease()] args", {
				holder: this.#lockHolderTaskID,
				releaser: taskID,
				componentIdx: this.#componentIdx
			});
			if (this.#lockHolderTaskID !== taskID) {
				__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#exclusiveRelease()] ignoring foreign release", {
					holder: this.#lockHolderTaskID,
					releaser: taskID,
					componentIdx: this.#componentIdx
				});
				return !1;
			}
			this.#lockHolderTaskID = null;
			this.#onExclusiveReleaseHandlers = this.#onExclusiveReleaseHandlers.filter((v) => !!v);
			for (let [idx, f] of this.#onExclusiveReleaseHandlers.entries()) try {
				this.#onExclusiveReleaseHandlers[idx] = null;
				f();
			} catch (err) {
				__jcoEmbeddedRuntime$debugLog("error while executing handler for next exclusive release", err);
				throw err;
			}
			this.#scheduleLockHandoff();
			return !0;
		}
		#scheduleLockHandoff() {
			if (this.#lockHandoffScheduled || this.#lockWaiters.length === 0) return;
			this.#lockHandoffScheduled = !0;
			queueMicrotask(() => {
				this.#lockHandoffScheduled = !1;
				if (this.#lockHolderTaskID !== null) return;
				let next = this.#lockWaiters.shift();
				if (!next) return;
				this.#lockHolderTaskID = next.taskID;
				next.resolve();
			});
		}
		onNextExclusiveRelease(fn) {
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#()onNextExclusiveRelease] registering");
			this.#onExclusiveReleaseHandlers.push(fn);
		}
		async waitForExclusiveRelease() {
			for (; this.isExclusivelyLocked();) await new Promise((resolve) => this.onNextExclusiveRelease(resolve));
		}
		#getSuspendedTaskMeta(taskID) {
			let metas = [...this.#suspendedThreadsByRef.values()].filter((meta) => meta.thread.taskID() === taskID);
			return metas.find((meta) => meta.cancellable && this.suspendedThreadReady(meta.thread)) ?? metas[0];
		}
		suspendTask(args) {
			let { task, ...opts } = args;
			task.endFirstSlice();
			return this.suspendThread({
				thread: task.defaultThread(),
				...opts
			});
		}
		resumeTaskByID(taskID) {
			let meta = this.#getSuspendedTaskMeta(taskID);
			return meta ? this.resumeThread(meta.thread) : !1;
		}
		suspendedTaskReady(taskID) {
			let meta = this.#getSuspendedTaskMeta(taskID);
			return meta ? this.suspendedThreadReady(meta.thread) : !1;
		}
		suspendedTaskCancellable(taskID) {
			return !!this.#getSuspendedTaskMeta(taskID)?.cancellable;
		}
		isTaskSuspended(taskID) {
			return !!this.#getSuspendedTaskMeta(taskID);
		}
		*suspendedTaskMetas() {
			for (let meta of this.#suspendedThreadsByRef.values()) yield {
				...meta,
				task: meta.thread.task()
			};
		}
		#removeSuspendedThreadMeta(thread) {
			let meta = this.#suspendedThreadsByRef.get(thread);
			this.#suspendedThreadsByRef.delete(thread);
			return meta;
		}
		suspendThread(args) {
			let { thread, readyFn, cancellable, onResume, suspended = !1 } = args;
			__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#suspendThread()]", {
				threadID: thread.id(),
				taskID: thread.taskID(),
				componentIdx: this.#componentIdx
			});
			if (thread.componentIdx() !== this.#componentIdx) throw Error("assert: thread component idx should match async state");
			if (this.#suspendedThreadsByRef.has(thread)) throw Error(`thread [${thread.id()}] already suspended`);
			let promise;
			let resume;
			if (onResume) resume = () => onResume(!thread.isCancelled() && !thread.isRejected() && !this.isTrapped());
			else {
				let resolvers = __jcoEmbeddedRuntime$promiseWithResolvers();
				promise = resolvers.promise;
				resume = () => resolvers.resolve(!thread.isCancelled() && !thread.isRejected() && !this.isTrapped());
			}
			if (storeTrap.stopped) {
				this.markTrapped(storeTrap.error);
				resume();
				return promise;
			}
			this.#suspendedThreadsByRef.set(thread, {
				thread,
				cancellable,
				readyFn,
				resume,
				suspended
			});
			thread.notifyProgress();
			this.runTickLoop();
			__jcoEmbeddedRuntime$checkForDeadlock(deadlock);
			return promise;
		}
		resumeThread(thread) {
			let meta = this.#removeSuspendedThreadMeta(thread);
			if (!meta) return !1;
			meta.resume();
			return !0;
		}
		markThreadReady(thread) {
			let meta = this.#suspendedThreadsByRef.get(thread);
			if (!meta || !meta.suspended) return !1;
			meta.suspended = !1;
			meta.readyFn = () => !0;
			this.runTickLoop();
			return !0;
		}
		isThreadSuspended(thread) {
			return this.#suspendedThreadsByRef.get(thread)?.suspended ?? !1;
		}
		isThreadReady(thread) {
			let meta = this.#suspendedThreadsByRef.get(thread);
			return !!meta && !meta.suspended && (meta.thread.isRejected() || meta.readyFn());
		}
		suspendedThreadReady(thread) {
			let meta = this.#suspendedThreadsByRef.get(thread);
			if (!meta) return !1;
			if (!meta.readyFn) throw Error(`suspended thread [${thread.id()}] is missing a readiness function`);
			if (this.isTrapped() || meta.thread.isRejected()) return !0;
			if (!meta.readyFn()) return !1;
			let task = meta.thread.task();
			return !task.needsExclusiveLock() || !this.isExclusivelyLocked() || this.exclusivelyLockedBy(task.id());
		}
		addPendingTaskStart() {
			this.#pendingTaskStarts++;
		}
		removePendingTaskStart() {
			this.#pendingTaskStarts--;
		}
		hasPendingSchedulerWork() {
			if (this.#pendingTaskStarts > 0) return !0;
			if (this.#lockHandoffScheduled) return !0;
			for (let meta of this.#suspendedThreadsByRef.values()) if (this.suspendedThreadReady(meta.thread)) return !0;
			return !1;
		}
		stop() {
			this.#tickLoop !== null && clearTimeout(this.#tickLoop);
			this.#tickLoop = null;
			this.abandonLockWaiters();
			this.abandonAdmissionWaiters();
			for (let thread of [...this.#suspendedThreadsByRef.keys()]) this.resumeThread(thread);
		}
		runTickLoop() {
			if (storeTrap.stopped || this.#tickLoop !== null) return;
			let run = () => {
				if (storeTrap.stopped) {
					this.#tickLoop = null;
					return;
				}
				let result = this.tick();
				if (storeTrap.stopped || result === __jcoEmbeddedRuntime$TICK_DONE) {
					this.#tickLoop = null;
					return;
				}
				result === __jcoEmbeddedRuntime$TICK_IDLE && __jcoEmbeddedRuntime$checkForDeadlock(deadlock);
				let delay = result === __jcoEmbeddedRuntime$TICK_RESUMED ? 0 : 10;
				this.#tickLoop = setTimeout(run, delay);
			};
			this.#tickLoop = setTimeout(run, 10);
		}
		tick() {
			for (let [thread, meta] of this.#suspendedThreadsByRef) {
				if (!meta || !meta.readyFn) throw Error(`missing/invalid thread meta despite thread [${thread.id()}] being present`);
				if (this.isTrapped() || meta.thread.isRejected()) {
					__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#tick()] detected thread rejection, leaving early", { meta });
					this.resumeThread(thread);
					return __jcoEmbeddedRuntime$TICK_RESUMED;
				}
				if (!this.suspendedThreadReady(thread)) continue;
				__jcoEmbeddedRuntime$debugLog("[ComponentAsyncState#tick()] resuming thread via tick", {
					threadID: thread.id(),
					componentIdx: this.#componentIdx
				});
				this.resumeThread(thread);
				return __jcoEmbeddedRuntime$TICK_RESUMED;
			}
			return this.#suspendedThreadsByRef.size > 0 ? __jcoEmbeddedRuntime$TICK_IDLE : __jcoEmbeddedRuntime$TICK_DONE;
		}
		createWaitable(args) {
			return new Waitable({ target: args?.target });
		}
	}
	return ComponentAsyncState;
}
const __jcoEmbeddedRuntime$postReturnTrap = (context, componentIdx, error) => {
	let trap;
	if (error instanceof context.RuntimeError) trap = error;
	else {
		let message = /call stack|stack overflow|too much recursion/i.test(error?.message ?? "") ? context.stackOverflow : context.uncaughtException;
		trap = new context.RuntimeError(message, { cause: error });
	}
	__jcoEmbeddedRuntime$getOrCreateAsyncState(context.lookup, componentIdx).markTrapped(trap);
	return trap;
};
function __jcoEmbeddedRuntime$getCurrentTask(_context, componentIdx, taskID) {
	if (componentIdx == null) throw Error("missing component idx");
	let taskMetas = _context.tasks.get(componentIdx);
	if (taskMetas === void 0 || taskMetas.length === 0) return;
	if (taskID) return taskMetas.find((meta) => meta.task.id() === taskID);
	let current = _context.registers[componentIdx];
	if (current && current.taskID !== void 0) {
		let currentMeta = taskMetas.find((meta) => meta.task.id() === current.taskID);
		if (currentMeta) return currentMeta;
	}
	let taskMeta = taskMetas[taskMetas.length - 1];
	if (!taskMeta || !taskMeta.task) return;
	return taskMeta;
}
function __jcoEmbeddedRuntime$createNewCurrentImplicitThread(_context, args) {
	__jcoEmbeddedRuntime$debugLog("[createNewCurrentImplicitThread()] args", args);
	let { componentIdx, isAsync, isManualAsync, preserveFutureResult, entryFnName, callbackFnName, getCallbackFn, stringEncoding, errHandling, getCalleeParamsFn, resultPtr, callingWasmExport } = args;
	if (componentIdx == null) throw Error("missing/invalid component instance index while starting task");
	let taskMetas = _context.tasks.get(componentIdx);
	let callbackFn = getCallbackFn ? getCallbackFn() : null;
	let newTask = new _context.AsyncTask({
		componentIdx,
		isAsync,
		isManualAsync,
		preserveFutureResult,
		entryFnName,
		callbackFn,
		callbackFnName,
		stringEncoding,
		getCalleeParamsFn,
		resultPtr,
		errHandling,
		callingWasmExport
	});
	let thread = newTask.addThread(new _context.AsyncThread({
		componentIdx,
		task: newTask
	}));
	let newTaskID = newTask.id();
	let newTaskMeta = {
		id: newTaskID,
		componentIdx,
		task: newTask
	};
	_context.taskIds.push(newTaskID);
	_context.componentIdxs.push(componentIdx);
	if (taskMetas) taskMetas.push(newTaskMeta);
	else {
		taskMetas = [newTaskMeta];
		_context.tasks.set(componentIdx, [newTaskMeta]);
	}
	return {
		task: newTask,
		taskID: newTaskID,
		thread,
		threadID: thread.id()
	};
}
function __jcoEmbeddedRuntime$clearCurrentThread(_context, componentIdx, taskID, options = {}) {
	__jcoEmbeddedRuntime$debugLog("[clearCurrentThread()] args", {
		componentIdx,
		taskID
	});
	if (componentIdx == null) throw Error("missing/invalid component instance index while ending current task");
	let tasks = _context.tasks.get(componentIdx);
	if (!tasks || !Array.isArray(tasks)) throw Error("missing/invalid tasks for component instance while ending task");
	if (tasks.length == 0) throw Error(`no current tasks for component instance [${componentIdx}] while ending task`);
	let taskIdx = taskID === void 0 ? tasks.length - 1 : tasks.findIndex((meta) => meta.id === taskID);
	if (taskIdx === -1) return;
	let taskMeta = tasks[taskIdx];
	if (!options.preserveTask) {
		tasks.splice(taskIdx, 1);
		_context.checkIdle?.();
	}
	let globalTaskIdx = _context.taskIds.lastIndexOf(taskMeta.id);
	if (globalTaskIdx !== -1) {
		_context.taskIds.splice(globalTaskIdx, 1);
		_context.componentIdxs.splice(globalTaskIdx, 1);
	}
	return taskMeta.task;
}
const __jcoEmbeddedRuntime$tableOwn = 2 ** 32;
const __jcoEmbeddedRuntime$initializeHandleIndexSpaces = () => new Map();
const __jcoEmbeddedRuntime$initializeResourceScopeCounter = () => ({ value: 0 });
const __jcoEmbeddedRuntime$initializeResourceScopeTasks = () => new Map();
const __jcoEmbeddedRuntime$getHandleIndexSpace = (spaces, key) => {
	let space = spaces.get(key);
	if (space === void 0) {
		space = {
			next: 1,
			free: [],
			alloc() {
				return this.free.length > 0 ? this.free.pop() : this.next++;
			},
			release(index) {
				this.free.push(index);
			}
		};
		spaces.set(key, space);
	}
	return space;
};
const __jcoEmbeddedRuntime$isInt8 = (value) => typeof value == "number" && value === value << 24 >> 24;
const __jcoEmbeddedRuntime$isUint8 = (value) => typeof value == "number" && value === (value & 255);
const __jcoEmbeddedRuntime$isInt16 = (value) => typeof value == "number" && value === value << 16 >> 16;
const __jcoEmbeddedRuntime$isUint16 = (value) => typeof value == "number" && value === (value & 65535);
const __jcoEmbeddedRuntime$isInt32 = (value) => typeof value == "number" && value === (value | 0);
const __jcoEmbeddedRuntime$isUint32 = (value) => typeof value == "number" && value === value >>> 0;
const __jcoEmbeddedRuntime$isInt64 = (value) => typeof value == "bigint" && value === BigInt.asIntN(64, value);
const __jcoEmbeddedRuntime$isUint64 = (value) => typeof value == "bigint" && value === BigInt.asUintN(64, value);
function __jcoEmbeddedRuntime$invalidNumericPrimitive(type, value) {
	throw TypeError(`invalid ${type} value [${value}]`);
}
const __jcoEmbeddedRuntime$toInt8 = (context, value) => {
	context.strict && !__jcoEmbeddedRuntime$isInt8(value) && __jcoEmbeddedRuntime$invalidNumericPrimitive("s8", value);
	return value << 24 >> 24;
};
const __jcoEmbeddedRuntime$toUint8 = (context, value) => {
	context.strict && !__jcoEmbeddedRuntime$isUint8(value) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u8", value);
	return value & 255;
};
const __jcoEmbeddedRuntime$toUint16 = (context, value) => {
	context.strict && !__jcoEmbeddedRuntime$isUint16(value) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u16", value);
	return value & 65535;
};
const __jcoEmbeddedRuntime$toUint32 = (context, value) => {
	context.strict && !__jcoEmbeddedRuntime$isUint32(value) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u32", value);
	return value >>> 0;
};
const __jcoEmbeddedRuntime$toUint64 = (context, value) => {
	let converted = BigInt(value);
	context.strict && !__jcoEmbeddedRuntime$isUint64(converted) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u64", converted);
	return BigInt.asUintN(64, converted);
};
const __jcoEmbeddedRuntime$isValidNumericPrimitive = (type, value) => {
	switch (type) {
		case "bool": return value === 0 || value === 1;
		case "s8": return __jcoEmbeddedRuntime$isInt8(value);
		case "u8": return __jcoEmbeddedRuntime$isUint8(value);
		case "s16": return __jcoEmbeddedRuntime$isInt16(value);
		case "u16": return __jcoEmbeddedRuntime$isUint16(value);
		case "s32": return __jcoEmbeddedRuntime$isInt32(value);
		case "u32": return __jcoEmbeddedRuntime$isUint32(value);
		case "s64": return __jcoEmbeddedRuntime$isInt64(value);
		case "u64": return __jcoEmbeddedRuntime$isUint64(value);
		case "f32":
		case "f64": return typeof value == "number";
		default: return !1;
	}
};
const __jcoEmbeddedRuntime$requireValidNumericPrimitive = (type, value) => {
	__jcoEmbeddedRuntime$isValidNumericPrimitive(type, value) || __jcoEmbeddedRuntime$invalidNumericPrimitive(type, value);
	return !0;
};
function __jcoEmbeddedRuntime$getCurrentThread(context, componentIdx, threadID, taskID) {
	if (componentIdx === -1 || threadID == null || threadID === 0) return;
	let thread = __jcoEmbeddedRuntime$getOrCreateAsyncState(context.lookup, componentIdx).threads.get(threadID);
	if (!thread || taskID !== void 0 && thread.taskID() !== taskID) return;
	return {
		id: threadID,
		componentIdx,
		taskID: thread.taskID(),
		task: thread.task(),
		thread
	};
}
const __jcoEmbeddedRuntime$emptyFunc = () => {};
const __jcoEmbeddedRuntime$initializeDataViewCache = () => ({ view: new DataView(new ArrayBuffer(0)) });
const __jcoEmbeddedRuntime$dataView = (context, memory) => context.view.buffer === memory.buffer ? context.view : context.view = new DataView(memory.buffer);
function __jcoEmbeddedRuntime$requireString(value) {
	if (typeof value != "string") throw TypeError("expected a string, received [" + typeof value + "]");
	return value;
}
function __jcoEmbeddedRuntime$utf8Size(s) {
	let len = 0;
	let codepoints = 0;
	for (let i = 0; i < s.length; i++) {
		let ch = s.charCodeAt(i);
		codepoints++;
		if (ch < 128) len += 1;
		else if (ch < 2048) len += 2;
		else if (ch >= 55296 && ch <= 56319 && i + 1 < s.length && (s.charCodeAt(i + 1) & 64512) == 56320) {
			len += 4;
			i++;
		} else len += 3;
	}
	return {
		len,
		codepoints
	};
}
const __jcoEmbeddedRuntime$initializeUtf8Decoder = () => new TextDecoder("utf-8", {
	fatal: !0,
	ignoreBOM: !0
});
const __jcoEmbeddedRuntime$initializeUtf16Decoder = () => new TextDecoder("utf-16", {
	fatal: !0,
	ignoreBOM: !0
});
const __jcoEmbeddedRuntime$initializeUtf8Encoder = () => new TextEncoder();
const __jcoEmbeddedRuntime$initializeUtf16Encoding = () => ({ littleEndian: new Uint8Array(new Uint16Array([1]).buffer)[0] === 1 });
function __jcoEmbeddedRuntime$writeUtf16(context, str, ptr, memory) {
	let len = str.length;
	let out = new Uint16Array(memory.buffer, ptr, len);
	let put = context.littleEndian ? (i, ch) => {
		out[i] = ch;
	} : (i, ch) => {
		out[i] = (ch & 255) << 8 | ch >>> 8;
	};
	for (let i = 0; i < len; i++) {
		let ch = str.charCodeAt(i);
		if ((ch & 63488) == 55296) {
			if (ch < 56320 && i + 1 < len && (str.charCodeAt(i + 1) & 64512) == 56320) {
				put(i++, ch);
				ch = str.charCodeAt(i);
			} else ch = 65533;
		}
		put(i, ch);
	}
	return {
		ptr,
		len,
		codepoints: [...str].length
	};
}
function __jcoEmbeddedRuntime$writeUtf8(encoder, s, ptr, len, codepoints, memory) {
	let { read, written } = encoder.encodeInto(s, new Uint8Array(memory.buffer, ptr, len));
	if (read !== s.length || written !== len) throw Error("failed to encode whole string");
	return {
		ptr,
		len,
		codepoints
	};
}
const __jcoEmbeddedRuntime$utf16Encode = (context, value, realloc, memory) => {
	let str = __jcoEmbeddedRuntime$requireString(value);
	return __jcoEmbeddedRuntime$writeUtf16(context, str, realloc(0, 0, 2, str.length * 2), memory);
};
const __jcoEmbeddedRuntime$utf8Encode = (encoder, value, realloc, memory) => {
	let s = __jcoEmbeddedRuntime$requireString(value);
	if (s.length === 0) return {
		ptr: 1,
		len: 0
	};
	let { len, codepoints } = __jcoEmbeddedRuntime$utf8Size(s);
	return __jcoEmbeddedRuntime$writeUtf8(encoder, s, realloc(0, 0, 1, len), len, codepoints, memory);
};
const __jcoEmbeddedRuntime$compactUtf16Decode = (decoder, memory, ptr, taggedLength) => {
	ptr >>>= 0;
	taggedLength >>>= 0;
	if (ptr % 2 != 0) throw RangeError("unaligned compact UTF-16 string");
	if (taggedLength & 2147483648) return decoder.decode(new Uint8Array(memory.buffer, ptr, (taggedLength & 2147483647) * 2));
	let bytes = new Uint8Array(memory.buffer, ptr, taggedLength);
	let value = "";
	for (let i = 0; i < bytes.length; i += 32768) value += String.fromCharCode(...bytes.subarray(i, i + 32768));
	return value;
};
const __jcoEmbeddedRuntime$compactUtf16Encode = (context, value, realloc, memory) => {
	if (typeof value != "string") throw TypeError("expected a string");
	if (value.length >= 2147483648) throw RangeError("compact UTF-16 string is too long");
	for (let i = 0; i < value.length; i++) if (value.charCodeAt(i) > 255) {
		let encoded = __jcoEmbeddedRuntime$utf16Encode(context, value, realloc, memory);
		return {
			...encoded,
			len: (encoded.len | 2147483648) >>> 0
		};
	}
	let ptr = realloc(0, 0, 2, value.length) >>> 0;
	if (ptr % 2 != 0) throw RangeError("unaligned compact UTF-16 allocation");
	let bytes = new Uint8Array(memory.buffer, ptr, value.length);
	for (let i = 0; i < value.length; i++) bytes[i] = value.charCodeAt(i);
	return {
		ptr,
		len: value.length
	};
};
function __jcoEmbeddedRuntime$initializeRepTableClass(context) {
	let { RuntimeError } = context;
	let free = Symbol("RepTable.free");
	class RepTable {
		static FREE = free;
		#data = [0, null];
		#size = 0;
		target;
		#space = null;
		constructor(args) {
			this.target = args?.target;
			this.#space = args?.space ?? null;
		}
		data() {
			return this.#data;
		}
		insert(val) {
			if (this.#space !== null) {
				let rep = this.#space.alloc();
				this.#data[rep << 1] = val;
				this.#data[(rep << 1) + 1] = null;
				this.#size += 1;
				return rep;
			}
			__jcoEmbeddedRuntime$debugLog("[RepTable#insert()] args", {
				val,
				target: this.target
			});
			let freeIdx = this.#data[0];
			if (freeIdx === 0) {
				this.#data.push(val);
				this.#data.push(null);
				let rep = (this.#data.length >> 1) - 1;
				__jcoEmbeddedRuntime$debugLog("[RepTable#insert()] inserted", {
					val,
					target: this.target,
					rep
				});
				this.#size += 1;
				return rep;
			}
			let placementIdx = freeIdx << 1;
			if (this.#data[placementIdx] !== free) throw Error("corrupt rep table freelist: head does not point at a freed slot");
			this.#data[0] = this.#data[placementIdx + 1];
			this.#data[placementIdx] = val;
			this.#data[placementIdx + 1] = null;
			__jcoEmbeddedRuntime$debugLog("[RepTable#insert()] inserted", {
				val,
				target: this.target,
				rep: freeIdx
			});
			this.#size += 1;
			return freeIdx;
		}
		get(rep) {
			__jcoEmbeddedRuntime$debugLog("[RepTable#get()] args", {
				rep,
				target: this.target
			});
			if (rep === 0) throw new RuntimeError("invalid resource rep during get, (cannot be 0)");
			let baseIdx = rep << 1;
			let val = this.#data[baseIdx];
			if (val === free) return;
			return val;
		}
		contains(rep) {
			__jcoEmbeddedRuntime$debugLog("[RepTable#contains()] args", {
				rep,
				target: this.target
			});
			if (rep === 0) throw Error("invalid resource rep during contains, (cannot be 0)");
			let baseIdx = rep << 1;
			let val = this.#data[baseIdx];
			return val !== free && !!val;
		}
		remove(rep) {
			__jcoEmbeddedRuntime$debugLog("[RepTable#remove()] args", {
				rep,
				target: this.target
			});
			if (rep === 0) throw Error("invalid resource rep during remove, (cannot be 0)");
			if (this.#space !== null) {
				let baseIdx = rep << 1;
				let val = this.#data[baseIdx];
				if (val === void 0 || val === free) throw new RuntimeError(`unknown handle index ${rep}`);
				this.#data[baseIdx] = free;
				this.#data[baseIdx + 1] = null;
				--this.#size;
				this.#space.release(rep);
				return val;
			}
			if (this.#data.length === 2) throw Error("invalid");
			let baseIdx = rep << 1;
			if (baseIdx >= this.#data.length) throw Error(`invalid rep [${rep}] during remove, out of range`);
			let val = this.#data[baseIdx];
			if (val === free) throw new RuntimeError(`double removal of rep [${rep}] (already freed)`);
			this.#data[baseIdx] = free;
			this.#data[baseIdx + 1] = this.#data[0];
			this.#data[0] = rep;
			--this.#size;
			return val;
		}
		size() {
			return this.#size;
		}
		clear() {
			__jcoEmbeddedRuntime$debugLog("[RepTable#clear()] args", { target: this.target });
			if (this.#space) for (let rep = 1; rep < this.#data.length / 2; rep++) this.#data[(rep << 1) + 1] === null && this.#data[rep << 1] !== free && this.#space.release(rep);
			this.#size = 0;
			this.#data = [0, null];
		}
	}
	return RepTable;
}
const __jcoEmbeddedRuntime$initializeAsyncDeterminism = (mode) => mode;
const __jcoEmbeddedRuntime$initializeRuntimeErrorConstructor = (platform) => platform.RuntimeError;
const __jcoEmbeddedRuntime$initializeComponentErrorClass = () => {
	class ComponentError extends Error {
		constructor(value) {
			let enumerable = typeof value != "string";
			super(enumerable ? `${String(value)} (see error.payload)` : value);
			Object.defineProperty(this, "payload", {
				value,
				enumerable
			});
		}
	}
	return ComponentError;
};
const __jcoEmbeddedRuntime$initializeSymbolDispose = () => Symbol.dispose || Symbol.for("dispose");
const __jcoEmbeddedRuntime$initializeEndianness = () => new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;
const __jcoEmbeddedRuntime$initializeHasOwnProperty = () => Object.prototype.hasOwnProperty;
const __jcoEmbeddedRuntime$initializeAsyncEventCodes = () => ({
	NONE: 0,
	SUBTASK: 1,
	STREAM_READ: 2,
	STREAM_WRITE: 3,
	FUTURE_READ: 4,
	FUTURE_WRITE: 5,
	TASK_CANCELLED: 6
});
const __jcoEmbeddedRuntime$initializeComponentMemoryMap = () => new Map();
const __jcoEmbeddedRuntime$coinFlip = () => Math.random() > .5;
const __jcoEmbeddedRuntime$clampGuest = (value, min, max) => {
	if (value < min || value > max) throw TypeError(`must be between ${min} and ${max}`);
	return value;
};
const __jcoEmbeddedRuntime$getErrorPayload = (hasOwnProperty, error) => {
	if (error && hasOwnProperty.call(error, "payload")) return error.payload;
	if (error instanceof Error) throw error;
	return error;
};
const __jcoEmbeddedRuntime$disposeHostValue = (disposeSymbol, value, disposeFn) => {
	if (!disposeFn) {
		if (value === null || typeof value != "object" && typeof value != "function") return;
		let disposable = value;
		if (typeof Symbol.asyncDispose == "symbol" && typeof disposable[Symbol.asyncDispose] == "function") disposeFn = disposable[Symbol.asyncDispose];
		else if (typeof disposable[disposeSymbol] == "function") disposeFn = disposable[disposeSymbol];
		else return;
	}
	let dispose = disposeFn;
	let report = (error) => console.error("[jco] error while disposing discarded host value", error);
	queueMicrotask(() => {
		try {
			let result = dispose.call(value);
			result && typeof result.then == "function" && result.then(void 0, report);
		} catch (error) {
			report(error);
		}
	});
};
const __jcoEmbeddedRuntime$throwInvalidBool = () => {
	throw TypeError("invalid variant discriminant for bool");
};
function __jcoEmbeddedRuntime$lookupMemories(map, args) {
	let { componentIdx } = args ?? {};
	if (args.componentIdx === void 0) throw TypeError("missing component idx");
	let metas = map.get(componentIdx);
	if (!metas) return [];
	return args.memoryIdx === void 0 ? Object.values(metas) : metas[args.memoryIdx]?.memory;
}
function __jcoEmbeddedRuntime$initializeWaitableClass() {
	class Waitable {
		#componentIdx;
		#pendingEventFn = null;
		#promise;
		#resolve;
		#reject;
		#waitableSet = null;
		#hasSyncWaiter = !1;
		#idx = null;
		target;
		constructor(args) {
			let { componentIdx } = args;
			this.#componentIdx = componentIdx;
			this.target = args.target;
			this.#resetPromise();
		}
		componentIdx() {
			return this.#componentIdx;
		}
		isInSet() {
			return this.#waitableSet !== null;
		}
		idx() {
			return this.#idx;
		}
		setIdx(idx) {
			if (idx === 0) throw Error("waitable idx cannot be zero");
			this.#idx = idx;
		}
		setTarget(tgt) {
			this.target = tgt;
		}
		#resetPromise() {
			let { promise, resolve, reject } = __jcoEmbeddedRuntime$promiseWithResolvers();
			this.#promise = promise;
			this.#resolve = resolve;
			this.#reject = reject;
		}
		resolve() {
			this.#resolve();
		}
		reject(err) {
			this.#reject(err);
		}
		promise() {
			return this.#promise;
		}
		hasPendingEvent() {
			return this.#pendingEventFn !== null;
		}
		setPendingEvent(fn) {
			__jcoEmbeddedRuntime$debugLog("[Waitable#setPendingEvent()] args", {
				waitable: this,
				inSet: this.#waitableSet
			});
			this.#pendingEventFn = fn;
		}
		getPendingEvent() {
			__jcoEmbeddedRuntime$debugLog("[Waitable#getPendingEvent()] args", {
				waitable: this,
				inSet: this.#waitableSet,
				hasPendingEvent: this.#pendingEventFn !== null
			});
			if (this.#pendingEventFn === null) return null;
			let eventFn = this.#pendingEventFn;
			this.#pendingEventFn = null;
			let e = eventFn();
			this.#resetPromise();
			return e;
		}
		join(waitableSet) {
			__jcoEmbeddedRuntime$debugLog("[Waitable#join()] args", {
				waitable: this,
				waitableSet,
				isRemoval: waitableSet === null
			});
			if (waitableSet === void 0) throw TypeError("waitable set must be not be undefined");
			this.#waitableSet && this.#waitableSet.removeWaitable(this);
			this.#waitableSet = waitableSet;
			waitableSet && waitableSet.addWaitable(this);
		}
		drop() {
			__jcoEmbeddedRuntime$debugLog("[Waitable#drop()] args", {
				componentIdx: this.#componentIdx,
				waitable: this
			});
			this.join(null);
		}
		async waitForPendingEvent(args) {
			let { cstate } = args;
			if (!cstate) throw TypeError("missing component state");
			if (this.#waitableSet !== null || this.#hasSyncWaiter) throw Error("waitable is already in a set/has a sync waiter");
			this.#hasSyncWaiter = !0;
			try {
				await cstate.waitUntil({
					cancellable: !1,
					readyFn: () => this.hasPendingEvent()
				});
			} finally {
				this.#hasSyncWaiter = !1;
			}
		}
	}
	return Waitable;
}
const __jcoEmbeddedRuntime$initializeTasksByComponent = () => new Map();
const __jcoEmbeddedRuntime$initializeCurrentTaskIds = () => [];
const __jcoEmbeddedRuntime$initializeCurrentComponentIdxs = () => [];
const __jcoEmbeddedRuntime$initializeCurrentTaskMayBlock = (platform) => {
	if (platform === !1) return !1;
	let Global = platform.Global;
	if (!Global) throw TypeError("async task registers require WebAssembly.Global");
	return new Global({
		value: "i32",
		mutable: !0
	}, 0);
};
const __jcoEmbeddedRuntime$TASK_INITIAL = "initial";
const __jcoEmbeddedRuntime$TASK_CANCEL_PENDING = "cancel-pending";
const __jcoEmbeddedRuntime$TASK_CANCEL_DELIVERED = "cancel-delivered";
const __jcoEmbeddedRuntime$TASK_RESOLVED = "resolved";
function __jcoEmbeddedRuntime$initializeAsyncTaskClass(context) {
	let { lookup: _lookup, registers: _registers, clearing: _clearing, RuntimeError: _RuntimeError, AsyncSubtask: _Subtask, Waitable: _Waitable, events: _events, determinism: _determinism, FutureValue, scopeCounter: _scopeCounter, scopeTasks: _scopeTasks, storeTrap: _storeTrap } = context;
	class AsyncTask {
		static _ID = 0n;
		static #hostThreadID = 0;
		static State = {
			INITIAL: __jcoEmbeddedRuntime$TASK_INITIAL,
			CANCELLED: "cancelled",
			CANCEL_PENDING: __jcoEmbeddedRuntime$TASK_CANCEL_PENDING,
			CANCEL_DELIVERED: __jcoEmbeddedRuntime$TASK_CANCEL_DELIVERED,
			RESOLVED: __jcoEmbeddedRuntime$TASK_RESOLVED
		};
		static BlockResult = {
			CANCELLED: "block.cancelled",
			NOT_CANCELLED: "block.not-cancelled"
		};
		#id;
		#componentIdx;
		#state;
		#isAsync;
		#isManualAsync;
		#callingWasmExport = !0;
		#lockFreeEntry = !1;
		#preserveFutureResult;
		#entryFnName;
		#onResolveHandlers = [];
		#progressWaiters = [];
		#completionPromise;
		#completionValue;
		#completionReady = !1;
		#settleCompletionPromise;
		#rejected = !1;
		#exitPromise;
		#resolveExitPromise;
		#memoryIdx = null;
		#memory = null;
		#callbackFn = null;
		#callbackFnName = null;
		#postReturnFn = null;
		#getCalleeParamsFn = null;
		#calleeIsAsync = null;
		#admissionTicket = null;
		#funcTypeIsAsync = !1;
		#returnResultTypeIdx = null;
		#parentSubtask = null;
		#errHandling;
		#returnLowerFns = null;
		#resourceScopeId;
		#resourceBorrowCount = 0;
		#resourceLenders = [];
		#resourceScopeExited = !1;
		#subtasks = [];
		#threads = [];
		#entered = !1;
		#exited = !1;
		#errored = null;
		get storage() {
			return this.defaultThread().storage;
		}
		cancelled = !1;
		cancelRequested = !1;
		alwaysTaskReturn = !1;
		returnCalls = 0;
		tmpRetI64HighBits = 0;
		constructor(opts) {
			this.#id = ++AsyncTask._ID;
			this.#resourceScopeId = ++_scopeCounter.value;
			_scopeTasks.set(this.#resourceScopeId, this);
			if (opts?.componentIdx === void 0) throw TypeError("missing component id during task creation");
			this.#componentIdx = opts.componentIdx;
			this.#state = __jcoEmbeddedRuntime$TASK_INITIAL;
			this.#isAsync = opts.isAsync ?? !1;
			this.#isManualAsync = opts.isManualAsync ?? !1;
			this.#preserveFutureResult = opts.preserveFutureResult ?? !1;
			this.#entryFnName = opts.entryFnName;
			this.#callingWasmExport = opts.callingWasmExport !== !1;
			let { promise: completionPromise, resolve: resolveCompletionPromise, reject: rejectCompletionPromise } = __jcoEmbeddedRuntime$promiseWithResolvers();
			this.#completionPromise = completionPromise;
			completionPromise.then(__jcoEmbeddedRuntime$emptyFunc, __jcoEmbeddedRuntime$emptyFunc);
			let completionSettled = !1;
			let settleCompletionPromise = () => {
				if (completionSettled || !this.#completionReady) return;
				completionSettled = !0;
				this.#errored === null ? this.#rejected ? rejectCompletionPromise(this.#completionValue) : this.#preserveFutureResult && this.#completionValue instanceof FutureValue ? this.#completionValue.resolveAsValue(resolveCompletionPromise) : resolveCompletionPromise(this.#completionValue) : rejectCompletionPromise(this.#errored);
			};
			this.#settleCompletionPromise = settleCompletionPromise;
			this.#onResolveHandlers.push((results) => {
				if (this.#parentSubtask !== null) return;
				if (!this.#isAsync && !this.#isManualAsync) return;
				this.#completionValue = results;
				this.#completionReady = !0;
			});
			let { promise: exitPromise, resolve: resolveExitPromise } = __jcoEmbeddedRuntime$promiseWithResolvers();
			this.#exitPromise = exitPromise;
			this.#resolveExitPromise = resolveExitPromise;
			opts.callbackFn && (this.#callbackFn = opts.callbackFn);
			opts.callbackFnName && (this.#callbackFnName = opts.callbackFnName);
			opts.getCalleeParamsFn && (this.#getCalleeParamsFn = opts.getCalleeParamsFn);
			opts.parentSubtask && (this.#parentSubtask = opts.parentSubtask);
			opts.errHandling && (this.#errHandling = opts.errHandling);
		}
		taskState() {
			return this.#state;
		}
		id() {
			return this.#id;
		}
		componentIdx() {
			return this.#componentIdx;
		}
		entryFnName() {
			return this.#entryFnName;
		}
		resourceScopeId() {
			return this.#resourceScopeId;
		}
		addBorrowedHandle() {
			if (this.#resourceScopeExited) throw Error("cannot add a borrow to an exited resource scope");
			this.#resourceBorrowCount++;
		}
		removeBorrowedHandle() {
			if (this.#resourceBorrowCount === 0) throw Error("resource borrow count underflow");
			this.#resourceBorrowCount--;
		}
		addResourceLender(table, handle) {
			if (this.#resourceScopeExited) throw Error("cannot add a lender to an exited resource scope");
			this.#resourceLenders.push({
				table,
				handle
			});
		}
		validateResourceBorrowScope() {
			if (this.#resourceScopeExited) return;
			if (_storeTrap.stopped) {
				this.#resourceLenders = [];
				this.#resourceScopeExited = !0;
				_scopeTasks.delete(this.#resourceScopeId);
				return;
			}
			if (this.#resourceBorrowCount !== 0) throw new _RuntimeError("borrow handles still remain at the end of the call");
			for (let { table, handle } of this.#resourceLenders) {
				let idx = handle << 1;
				let lendCount = table[idx];
				if (!Number.isInteger(lendCount) || lendCount <= 0 || lendCount >= 2 ** 30) throw Error("invalid resource lender state at scope exit");
				table[idx] = lendCount - 1;
			}
			this.#resourceLenders = [];
			this.#resourceScopeExited = !0;
			_scopeTasks.delete(this.#resourceScopeId);
		}
		completionPromise() {
			return this.#completionPromise;
		}
		settleCompletion() {
			this.#settleCompletionPromise();
		}
		exitPromise() {
			return this.#exitPromise;
		}
		waitForProgress() {
			let { promise, resolve } = __jcoEmbeddedRuntime$promiseWithResolvers();
			this.#progressWaiters.push(resolve);
			return promise;
		}
		notifyProgress() {
			let waiters = this.#progressWaiters;
			this.#progressWaiters = [];
			for (let resolve of waiters) resolve();
		}
		isAsync() {
			return this.#isAsync;
		}
		isManualAsync() {
			return this.#isManualAsync;
		}
		isSync() {
			return !this.isAsync();
		}
		getErrHandling() {
			return this.#errHandling;
		}
		hasCallback() {
			return this.#callbackFn !== null;
		}
		getReturnMemoryIdx() {
			return this.#memoryIdx;
		}
		setReturnMemoryIdx(idx) {
			if (idx === null) return;
			this.#memoryIdx = idx;
		}
		getReturnMemory() {
			return this.#memory;
		}
		setReturnMemory(m) {
			if (m === null) return;
			this.#memory = m;
		}
		setReturnLowerFns(fns) {
			this.#returnLowerFns = fns;
		}
		getReturnLowerFns() {
			return this.#returnLowerFns;
		}
		setCalleeIsAsync(value) {
			if (typeof value != "boolean") throw TypeError("callee async state must be a boolean");
			this.#calleeIsAsync = value;
		}
		setParentSubtask(subtask) {
			if (!subtask || !(subtask instanceof _Subtask)) return;
			if (this.#parentSubtask) throw Error("parent subtask can only be set once");
			this.#parentSubtask = subtask;
		}
		getParentSubtask() {
			return this.#parentSubtask;
		}
		getRootTask() {
			let currentSubtask = this.getParentSubtask();
			let task = this;
			for (; currentSubtask;) {
				task = currentSubtask.getParentTask();
				currentSubtask = task.getParentSubtask();
			}
			return task;
		}
		setPostReturnFn(f) {
			if (!f) return;
			if (this.#postReturnFn) throw Error("postReturn fn can only be set once");
			this.#postReturnFn = f;
		}
		setCallbackFn(f, name) {
			if (!f) return;
			if (this.#callbackFn) throw Error("callback fn can only be set once");
			this.#callbackFn = f;
			this.#callbackFnName = name;
		}
		getCallbackFnName() {
			if (!this.#callbackFnName) return;
			return this.#callbackFnName;
		}
		runCallbackFn(...args) {
			if (!this.#callbackFn) throw Error("no callback function has been set for task");
			if (this.#callbackFn._jcoMaySuspend === !1) return __jcoEmbeddedRuntime$withCurrentTaskMeta(_registers, {
				taskID: this.#id,
				threadID: this.defaultThread().id(),
				componentIdx: this.#componentIdx,
				fn: () => this.#callbackFn.apply(null, args)
			});
			return __jcoEmbeddedRuntime$withCurrentTaskMetaAsync(_registers, {
				taskID: this.#id,
				threadID: this.defaultThread().id(),
				componentIdx: this.#componentIdx,
				fn: () => this.#callbackFn.apply(null, args)
			});
		}
		getCalleeParams() {
			if (!this.#getCalleeParamsFn) throw Error("missing/invalid getCalleeParamsFn");
			return this.#getCalleeParamsFn();
		}
		mayBlock() {
			return this.isAsync() || this.isManualAsync() || this.isResolvedState() || this.#funcTypeIsAsync;
		}
		setFuncTypeIsAsync(v) {
			this.#funcTypeIsAsync = v;
		}
		setReturnResultTypeIdx(v) {
			this.#returnResultTypeIdx = v;
		}
		returnResultTypeIdx() {
			return this.#returnResultTypeIdx;
		}
		mayEnter(task) {
			let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			if (cstate.hasBackpressure()) {
				__jcoEmbeddedRuntime$debugLog("[AsyncTask#mayEnter()] disallowed due to backpressure", { taskID: this.#id });
				return !1;
			}
			if (!cstate.callingSyncImport()) {
				__jcoEmbeddedRuntime$debugLog("[AsyncTask#mayEnter()] disallowed due to sync import call", { taskID: this.#id });
				return !1;
			}
			if (!Reflect.get(cstate, "callingSyncExport") || task.isAsync) {
				__jcoEmbeddedRuntime$debugLog("[AsyncTask#mayEnter()] disallowed due to sync export w/ sync pending", { taskID: this.#id });
				return !1;
			}
			return !0;
		}
		enterSync() {
			if (this.needsExclusiveLock()) {
				let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
				if (!cstate.isExclusivelyLocked()) cstate.exclusiveLock(this.#id);
				else {
					this.#lockFreeEntry = !0;
					__jcoEmbeddedRuntime$debugLog("[AsyncTask#enterSync()] entering without exclusive lock", {
						taskID: this.#id,
						componentIdx: this.#componentIdx
					});
				}
			}
			return !0;
		}
		tryEnter() {
			if (this.#entered) throw Error(`task with ID [${this.#id}] should not be entered twice`);
			if (this.deliverPendingCancel({ cancellable: !0 })) {
				this.cancel();
				return !1;
			}
			let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			if (this.isSync()) {
				this.#entered = !0;
				return !0;
			}
			if (cstate.hasBackpressure()) return null;
			if (cstate.hasPendingAdmissions()) return null;
			if (this.needsExclusiveLock()) {
				if (cstate.isExclusivelyLocked()) return null;
				cstate.exclusiveLock(this.#id);
			}
			if (this.deliverPendingCancel({ cancellable: !0 })) {
				cstate.exclusiveRelease(this.#id);
				this.cancel();
				return !1;
			}
			this.#entered = !0;
			return !0;
		}
		async enter(opts) {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#enter()] args", {
				taskID: this.#id,
				componentIdx: this.#componentIdx,
				subtaskID: this.getParentSubtask()?.id(),
				args: opts,
				entryFnName: this.#entryFnName
			});
			if (opts?.isHost) return this.enterHost();
			if (this.#entered) throw Error(`task with ID [${this.#id}] should not be entered twice`);
			if (this.deliverPendingCancel({ cancellable: !0 })) {
				this.cancel();
				return !1;
			}
			let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			if (this.isSync()) {
				this.#entered = !0;
				this.#isManualAsync && this.needsExclusiveLock() && await cstate.acquireExclusiveLock(this.#id);
				return this.#entered;
			}
			let ticket = cstate.takeAdmissionTicket(this.#id);
			this.#admissionTicket = ticket;
			let entered = !1;
			try {
				entered = await this.#enterQueued(cstate, ticket);
				return entered;
			} finally {
				entered || this.endFirstSlice();
			}
		}
		endFirstSlice() {
			let ticket = this.#admissionTicket;
			if (ticket === null) return;
			this.#admissionTicket = null;
			__jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx).releaseAdmissionTicket(ticket);
		}
		async #enterQueued(cstate, ticket) {
			await cstate.waitForAdmissionTurn(ticket);
			if (this.isResolvedState() || this.isCancelled()) return !1;
			for (;;) {
				if (cstate.hasBackpressure()) {
					cstate.addBackpressureWaiter();
					let result = await this.waitUntil({
						readyFn: () => !cstate.hasBackpressure(),
						cancellable: !0
					});
					cstate.removeBackpressureWaiter();
					if (!result || this.isCancelled()) {
						this.isResolvedState() || this.cancel();
						return !1;
					}
				}
				if (this.needsExclusiveLock()) {
					await cstate.acquireExclusiveLock(this.#id);
					if (cstate.hasBackpressure()) {
						cstate.exclusiveRelease(this.#id);
						continue;
					}
				}
				break;
			}
			try {
				cstate.throwIfInterrupted();
			} catch (err) {
				cstate.exclusiveRelease(this.#id);
				this.setErrored(err);
				return !1;
			}
			if (this.isResolvedState() || this.isCancelled()) {
				cstate.exclusiveRelease(this.#id);
				return !1;
			}
			if (this.deliverPendingCancel({ cancellable: !0 })) {
				cstate.exclusiveRelease(this.#id);
				this.cancel();
				return !1;
			}
			this.#entered = !0;
			return this.#entered;
		}
		isRunningState() {
			return this.#state !== __jcoEmbeddedRuntime$TASK_RESOLVED;
		}
		isResolvedState() {
			return this.#state === __jcoEmbeddedRuntime$TASK_RESOLVED;
		}
		isResolved() {
			return this.#state === __jcoEmbeddedRuntime$TASK_RESOLVED;
		}
		isExited() {
			return this.#exited;
		}
		async waitUntil(opts) {
			let { readyFn, cancellable } = opts;
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#waitUntil()] args", {
				taskID: this.#id,
				args: { cancellable }
			});
			return await this.suspendUntil({
				readyFn,
				cancellable
			});
		}
		async yieldUntil(opts) {
			let { readyFn, cancellable } = opts;
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#yieldUntil()]", {
				taskID: this.#id,
				args: { cancellable },
				componentIdx: this.#componentIdx
			});
			return {
				code: await this.immediateSuspend({
					readyFn,
					cancellable
				}) ? _events.NONE : _events.TASK_CANCELLED,
				payload0: 0,
				payload1: 0
			};
		}
		async suspendUntil(opts) {
			let { cancellable, readyFn } = opts;
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#suspendUntil()] args", {
				taskID: this.#id,
				args: { cancellable },
				componentIdx: this.#componentIdx
			});
			if (this.deliverPendingCancel({ cancellable })) return !1;
			return await this.immediateSuspendUntil({
				readyFn,
				cancellable
			});
		}
		suspendUntilCallback(opts, onResume) {
			let { cancellable, readyFn } = opts;
			if (this.deliverPendingCancel({ cancellable })) {
				onResume(!1);
				return;
			}
			__jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx).suspendTask({
				task: this,
				cancellable: !!cancellable,
				readyFn: () => {
					if (cancellable && this.#state === __jcoEmbeddedRuntime$TASK_CANCEL_PENDING) return !0;
					return readyFn();
				},
				onResume: (keepGoing) => {
					keepGoing && this.deliverPendingCancel({ cancellable }) && (keepGoing = !1);
					onResume(keepGoing);
				}
			});
		}
		async immediateSuspendUntil(opts) {
			let { cancellable, readyFn } = opts;
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#immediateSuspendUntil()] args", {
				args: {
					cancellable,
					readyFn
				},
				taskID: this.#id,
				componentIdx: this.#componentIdx
			});
			if (readyFn() && _determinism === "random" && __jcoEmbeddedRuntime$coinFlip()) return !0;
			return await this.immediateSuspend({
				cancellable,
				readyFn
			});
		}
		async immediateSuspend(opts) {
			let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			let thread = this.#entered ? cstate.threads.get(__jcoEmbeddedRuntime$getCurrentTaskMeta(_registers, this.#componentIdx).threadID) : this.defaultThread();
			if (!thread || thread.task() !== this) throw Error("invalid current thread for wait");
			return thread.suspend({
				...opts,
				releaseLock: !1
			});
		}
		deliverPendingCancel(opts) {
			let { cancellable } = opts;
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#deliverPendingCancel()]", {
				args: { cancellable },
				taskID: this.#id,
				componentIdx: this.#componentIdx
			});
			if (cancellable && this.#state === __jcoEmbeddedRuntime$TASK_CANCEL_PENDING) {
				this.#state = __jcoEmbeddedRuntime$TASK_CANCEL_DELIVERED;
				return !0;
			}
			return !1;
		}
		isCancelled() {
			return this.cancelled;
		}
		cancellationRequested() {
			return this.cancelRequested;
		}
		requestCancellation() {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#requestCancellation()] args", {
				taskID: this.#id,
				componentIdx: this.#componentIdx,
				state: this.#state
			});
			if (this.isResolvedState() || this.cancelRequested) return;
			this.cancelRequested = !0;
			if (this.#state === __jcoEmbeddedRuntime$TASK_INITIAL) {
				this.#state = __jcoEmbeddedRuntime$TASK_CANCEL_PENDING;
				if (!this.#entered) {
					this.deliverPendingCancel({ cancellable: !0 });
					this.cancel();
					let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
					cstate.resumeTaskByID(this.#id);
					cstate.cancelExclusiveLockWaiter(this.#id);
					this.exit({ skipExclusiveLockCheck: !0 });
					return;
				}
			}
			__jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx).runTickLoop();
		}
		cancel(args) {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#cancel()] args", {});
			if (this.taskState() !== __jcoEmbeddedRuntime$TASK_CANCEL_DELIVERED) throw Error("`task.cancel` called by task which has not been cancelled");
			this.validateResourceBorrowScope();
			this.cancelled = !0;
			this.onResolve(args?.error ?? null);
			this.#state = __jcoEmbeddedRuntime$TASK_RESOLVED;
			this.#entered || this.notifyProgress();
		}
		onResolve(taskValue) {
			let handlers = this.#onResolveHandlers;
			this.#onResolveHandlers = [];
			for (let f of handlers) try {
				f(taskValue);
			} catch (err) {
				__jcoEmbeddedRuntime$debugLog("[AsyncTask#onResolve] error during task resolve handler", err);
				throw err;
			}
			if (this.#rejected) {
				this.#parentSubtask?.reject(taskValue);
				return;
			}
			let parentSubtaskPending = this.#parentSubtask && !this.#parentSubtask.isResolved();
			let taskReturned = !this.isCancelled();
			if (parentSubtaskPending && taskReturned) {
				let meta = this.#parentSubtask.getCallMetadata();
				if (meta.returnFn && !meta.returnFnCalled) {
					__jcoEmbeddedRuntime$debugLog("[AsyncTask#onResolve()] running returnFn", {
						componentIdx: this.#componentIdx,
						taskID: this.#id,
						subtaskID: this.#parentSubtask.id()
					});
					let callerTask = this.#parentSubtask.getParentTask();
					__jcoEmbeddedRuntime$withCurrentTaskMeta(_registers, {
						taskID: callerTask.id(),
						threadID: meta.callerThreadID,
						componentIdx: callerTask.componentIdx(),
						fn: () => meta.returnFn.apply(null, [taskValue, meta.resultPtr])
					});
					meta.returnFnCalled = !0;
				}
			}
			if (this.#postReturnFn && taskReturned) {
				__jcoEmbeddedRuntime$debugLog("[AsyncTask#onResolve()] running post return ", {
					componentIdx: this.#componentIdx,
					taskID: this.#id
				});
				try {
					__jcoEmbeddedRuntime$withCurrentTaskMeta(_registers, {
						taskID: this.#id,
						threadID: this.defaultThread().id(),
						componentIdx: this.#componentIdx,
						fn: () => this.#postReturnFn(taskValue)
					});
				} catch (err) {
					err = __jcoEmbeddedRuntime$postReturnTrap(context.postReturn, this.#componentIdx, err);
					this.reject(err);
					throw err;
				}
			}
			parentSubtaskPending && this.#parentSubtask.onResolve(taskValue);
		}
		registerOnResolveHandler(f) {
			this.#onResolveHandlers.push(f);
		}
		isRejected() {
			return this.#rejected;
		}
		isErrored() {
			return this.#errored;
		}
		setErrored(err) {
			this.#errored === null && (this.#errored = err);
		}
		stop(err) {
			this.reject(err);
			this.#completionReady = !0;
			this.#settleCompletionPromise();
		}
		reject(taskErr) {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#reject()] args", {
				componentIdx: this.#componentIdx,
				taskID: this.#id,
				parentSubtask: this.#parentSubtask,
				parentSubtaskID: this.#parentSubtask?.id(),
				entryFnName: this.entryFnName(),
				callbackFnName: this.#callbackFnName,
				errMsg: taskErr.message
			});
			this.setErrored(taskErr);
			if (this.#rejected) return;
			if (this.isResolvedState()) {
				this.#rejected = !0;
				this.#errored = taskErr;
				let parentTask = this.#parentSubtask?.getParentTask();
				parentTask && parentTask.reject(taskErr);
				return;
			}
			this.#rejected = !0;
			this.cancelRequested = !0;
			this.#state = __jcoEmbeddedRuntime$TASK_CANCEL_PENDING;
			this.deliverPendingCancel({ cancellable: !0 });
			this.cancel({ error: taskErr });
		}
		resolve(results) {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#resolve()] args", {
				componentIdx: this.#componentIdx,
				taskID: this.#id,
				entryFnName: this.entryFnName(),
				callbackFnName: this.#callbackFnName
			});
			if (this.#state === __jcoEmbeddedRuntime$TASK_RESOLVED) {
				if (this.#errored !== null) throw this.#errored;
				throw Error(`(component [${this.#componentIdx}]) task [${this.#id}]  is already resolved (did you forget to wait for an import?)`);
			}
			this.validateResourceBorrowScope();
			this.#state = __jcoEmbeddedRuntime$TASK_RESOLVED;
			switch (results.length) {
				case 0:
					this.onResolve(void 0);
					break;
				case 1:
					this.onResolve(results[0]);
					break;
				default:
					__jcoEmbeddedRuntime$debugLog("[AsyncTask#resolve()] unexpected number of results", {
						componentIdx: this.#componentIdx,
						results,
						taskID: this.#id,
						subtaskID: this.#parentSubtask?.id(),
						entryFnName: this.#entryFnName,
						callbackFnName: this.#callbackFnName
					});
					throw Error("unexpected number of results");
			}
		}
		exit(args) {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#exit()]", {
				componentIdx: this.#componentIdx,
				taskID: this.#id
			});
			this.endFirstSlice();
			if (this.#exited) throw Error("task has already exited");
			this.#checkThreadExit();
			this.validateResourceBorrowScope();
			let state = this.#componentIdx === -1 ? null : __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			if (!state && this.#componentIdx !== -1) throw Error("missing async state for component [" + this.#componentIdx + "]");
			if (state && !args?.skipExclusiveLockCheck && !this.#lockFreeEntry && this.needsExclusiveLock() && !state.exclusivelyLockedBy(this.#id)) throw Error(`task [${this.#id}] exit: component [${this.#componentIdx}] should have been exclusively locked by it`);
			state && state.exclusiveRelease(this.#id);
			this.notifyProgress();
			try {
				this.#parentSubtask === null && (this.#isAsync || this.#isManualAsync) && this.#settleCompletionPromise();
				this.#resolveExitPromise();
			} catch (err) {
				console.error("error during task exit handler", err);
				throw err;
			}
			this.#exited = !0;
			this.removeThread(this.defaultThread());
			this.#threads.length > 0 && __jcoEmbeddedRuntime$clearCurrentThread(_clearing, this.#componentIdx, this.#id, { preserveTask: !0 });
		}
		needsExclusiveLock() {
			if (this.#componentIdx === -1) return !1;
			if (!this.#callingWasmExport) return !1;
			if (!this.#funcTypeIsAsync) return !1;
			return !this.#isAsync || this.hasCallback() || this.#calleeIsAsync === !1;
		}
		createSubtask(args) {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#createSubtask()] args", args);
			let { componentIdx, childTask, callMetadata, fnName, isAsync, isManualAsync } = args;
			if (!__jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx)) throw Error(`invalid/missing async state for component idx [${componentIdx}]`);
			let waitable = new _Waitable({
				componentIdx: this.#componentIdx,
				target: `subtask (internal ID [${this.#id}])`
			});
			let newSubtask = new _Subtask({
				componentIdx,
				childTask,
				parentTask: this,
				callMetadata,
				isAsync,
				isManualAsync,
				fnName,
				waitable
			});
			this.#subtasks.push(newSubtask);
			newSubtask.setTarget(`subtask (internal ID [${newSubtask.id()}], waitable [${waitable.idx()}], component [${componentIdx}])`);
			return newSubtask;
		}
		getLatestSubtask() {
			return this.#subtasks.at(-1);
		}
		getSubtaskByWaitableRep(rep) {
			if (rep === void 0) throw TypeError("missing rep");
			return this.#subtasks.find((s) => s.waitableRep() === rep);
		}
		currentSubtask() {
			__jcoEmbeddedRuntime$debugLog("[AsyncTask#currentSubtask()]");
			if (this.#subtasks.length === 0) return;
			return this.#subtasks.at(-1);
		}
		removeSubtask(subtask) {
			if (this.#subtasks.length === 0) throw Error("cannot end current subtask: no current subtask");
			this.#subtasks = this.#subtasks.filter((t) => t !== subtask);
			return subtask;
		}
		cancellationPending() {
			return this.#state === __jcoEmbeddedRuntime$TASK_CANCEL_PENDING;
		}
		enterHost() {
			if (this.#entered) throw Error(`task with ID [${this.#id}] should not be entered twice`);
			if (this.deliverPendingCancel({ cancellable: !0 })) {
				this.cancel();
				return !1;
			}
			__jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			this.#entered = !0;
			let parentTask = this.#parentSubtask?.getParentTask();
			(parentTask?.taskState() === __jcoEmbeddedRuntime$TASK_CANCEL_DELIVERED || parentTask && !parentTask.hasCallback()) && parentTask.notifyProgress();
			return this.#entered;
		}
		defaultThread() {
			return this.#threads[0];
		}
		removeThread(thread) {
			this.#checkThreadExit();
			this.settleCompletion();
			this.#componentIdx !== -1 && __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx).threads.remove(thread.id());
			this.#threads = this.#threads.filter((t) => t !== thread);
			this.#exited && this.#threads.length === 0 && __jcoEmbeddedRuntime$clearCurrentThread(_clearing, this.#componentIdx, this.#id);
		}
		#checkThreadExit() {
			if (this.#threads.length === 1 && !this.isResolvedState() && !this.isRejected()) {
				let err = new _RuntimeError(`(component [${this.#componentIdx}]) task [${this.#id}] exited without resolution`);
				throw this.#componentIdx === -1 ? err : __jcoEmbeddedRuntime$recordTrap(context.traps, this.#componentIdx, err);
			}
		}
		addThread(thread) {
			if (this.#componentIdx === -1) {
				thread.setId(++AsyncTask.#hostThreadID);
				this.#threads.push(thread);
				return thread;
			}
			let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			thread.setId(cstate.threads.insert(thread));
			this.#threads.push(thread);
			return thread;
		}
		calleeIsAsync() {
			return this.#calleeIsAsync === !0;
		}
	}
	return AsyncTask;
}
function __jcoEmbeddedRuntime$initializeAsyncSubtaskClass(context) {
	let { lookup: _lookup, memories: _memories, disposeSymbol: _disposeSymbol, RuntimeError } = context;
	class AsyncSubtask {
		static _ID = 0n;
		static State = {
			STARTING: 0,
			STARTED: 1,
			RETURNED: 2,
			CANCELLED_BEFORE_STARTED: 3,
			CANCELLED_BEFORE_RETURNED: 4
		};
		#id;
		#state = 0;
		#componentIdx;
		#parentTask;
		#childTask = null;
		#cancelRequested = !1;
		#lenders = null;
		#waitable;
		#callbackFn = null;
		#postReturnFn = null;
		#onProgressFn = null;
		#callMetadata = {};
		#resolved = !1;
		#onResolveHandlers = [];
		#onStartHandlers = [];
		#result = null;
		#resultSet = !1;
		#hostPendingResult = null;
		fnName;
		target;
		isAsync;
		isManualAsync;
		cancelProgress = null;
		constructor(args) {
			if (typeof args.componentIdx != "number") throw Error("invalid componentIdx for subtask creation");
			this.#componentIdx = args.componentIdx;
			this.#id = ++AsyncSubtask._ID;
			this.fnName = args.fnName;
			if (!args.parentTask) throw Error("missing parent task during subtask creation");
			this.#parentTask = args.parentTask;
			args.childTask && (this.#childTask = args.childTask);
			if (!args.waitable) throw Error("missing/invalid waitable");
			this.#waitable = args.waitable;
			args.callMetadata && (this.#callMetadata = args.callMetadata);
			this.#lenders = [];
			this.target = args.target;
			this.isAsync = args.isAsync;
			this.isManualAsync = args.isManualAsync;
		}
		id() {
			return this.#id;
		}
		parentTaskID() {
			return this.#parentTask?.id();
		}
		childTaskID() {
			return this.#childTask?.id();
		}
		state() {
			return this.#state;
		}
		getWaitable() {
			return this.#waitable;
		}
		waitable() {
			return this.#waitable;
		}
		waitableRep() {
			return this.#waitable.idx();
		}
		ensureHandle() {
			let existing = this.#waitable.idx();
			if (existing != null) return existing;
			let idx = this.#getComponentState().handles.insert(this);
			this.#waitable.setIdx(idx);
			this.#waitable.setTarget(`waitable for subtask (waitable id [${idx}], subtask internal ID [${this.id()}])`);
			return idx;
		}
		join(set) {
			return this.#waitable.join(set);
		}
		getPendingEvent() {
			return this.#waitable.getPendingEvent();
		}
		hasPendingEvent() {
			return this.#waitable.hasPendingEvent();
		}
		setPendingEvent(fn) {
			return this.#waitable.setPendingEvent(fn);
		}
		setTarget(tgt) {
			this.target = tgt;
		}
		getResult() {
			if (!this.#resultSet) throw Error("subtask result has not been set");
			return this.#result;
		}
		setResult(v) {
			if (this.#resultSet) throw Error("subtask result has already been set");
			this.#result = v;
			this.#resultSet = !0;
		}
		componentIdx() {
			return this.#componentIdx;
		}
		setChildTask(t) {
			if (!t) throw Error("cannot set missing/invalid child task on subtask");
			if (this.#childTask) throw Error("child task is already set on subtask");
			if (this.#parentTask === t) throw Error("parent cannot be child");
			this.#childTask = t;
		}
		getChildTask() {
			return this.#childTask;
		}
		getParentTask() {
			return this.#parentTask;
		}
		setCallbackFn(f, _name) {
			if (!f) return;
			if (this.#callbackFn) throw Error("callback fn can only be set once");
			this.#callbackFn = f;
		}
		getCallbackFnName() {
			return this.#callbackFn?.name;
		}
		setPostReturnFn(f) {
			if (!f) return;
			if (this.#postReturnFn) throw Error("postReturn fn can only be set once");
			this.#postReturnFn = f;
		}
		setOnProgressFn(f) {
			if (this.#onProgressFn) throw Error("on progress fn can only be set once");
			this.#onProgressFn = f;
		}
		isNotStarted() {
			return this.#state == 0;
		}
		cancellationRequested() {
			return this.#cancelRequested;
		}
		requestCancellation() {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#requestCancellation()] args", {
				componentIdx: this.#componentIdx,
				subtaskID: this.#id,
				state: this.#state,
				childTaskID: this.childTaskID(),
				fnName: this.fnName
			});
			if (this.#cancelRequested) throw Error("cancellation has already been requested for this subtask");
			this.#cancelRequested = !0;
			if (this.#resolved) return;
			if (this.#childTask) {
				this.#childTask.requestCancellation();
				return;
			}
			this.onResolve(null);
			this.#disposeHostPendingResult();
		}
		setHostPendingResult(v) {
			if (!__jcoEmbeddedRuntime$isPromiseLike(v)) return;
			this.#hostPendingResult = v;
			this.#resolved && this.#state !== 2 && this.#disposeHostPendingResult();
		}
		#disposeHostPendingResult() {
			let pending = this.#hostPendingResult;
			if (!pending) return;
			this.#hostPendingResult = null;
			__jcoEmbeddedRuntime$disposeHostValue(_disposeSymbol, pending);
		}
		registerOnStartHandler(f) {
			this.#onStartHandlers.push(f);
		}
		onStart(args) {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#onStart()] args", {
				componentIdx: this.#componentIdx,
				subtaskID: this.#id,
				parentTaskID: this.parentTaskID(),
				fnName: this.fnName,
				args
			});
			this.#onProgressFn?.();
			this.#parentTask.notifyProgress();
			this.#state = 1;
			let result;
			this.#callMetadata.startFn && (result = this.#callMetadata.startFn.apply(null, args?.startFnParams ?? []));
			return result;
		}
		registerOnResolveHandler(f) {
			this.#onResolveHandlers.push(f);
		}
		reject(subtaskErr) {
			if (this.#resolved) return;
			this.#onProgressFn?.();
			if (this.#state >= 2) throw Error("cannot reject a completed subtask");
			this.#state = this.#state === 0 ? 3 : 4;
			this.#resolved = !0;
			this.#hostPendingResult = null;
			this.#parentTask.removeSubtask(this);
			this.#parentTask.reject(subtaskErr);
		}
		onResolve(subtaskValue) {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#onResolve()] args", {
				componentIdx: this.#componentIdx,
				subtaskID: this.#id,
				isAsync: this.isAsync,
				childTaskID: this.childTaskID(),
				parentTaskID: this.parentTaskID(),
				parentTaskFnName: this.#parentTask?.entryFnName(),
				fnName: this.fnName
			});
			if (this.#resolved) throw Error("subtask has already been resolved");
			this.#onProgressFn?.();
			if (subtaskValue === null && this.#cancelRequested) {
				if (this.#state >= 2) throw Error("resolved subtask must have been started before cancellation");
				this.#state = this.#state === 0 ? 3 : 4;
			} else {
				if (this.#state !== 1) throw Error("resolved subtask must have been started before completion");
				this.#state = 2;
				this.#hostPendingResult = null;
			}
			this.setResult(subtaskValue);
			for (let f of this.#onResolveHandlers) try {
				f(subtaskValue);
			} catch (err) {
				console.error("error during subtask resolve handler", err);
				throw err;
			}
			let callMetadata = this.getCallMetadata();
			let memory = callMetadata.memory ?? this.#parentTask?.getReturnMemory() ?? __jcoEmbeddedRuntime$lookupMemories(_memories, { componentIdx: this.#parentTask.componentIdx() })[0]?.memory;
			if (this.#state === 2 && callMetadata && !callMetadata.returnFn && (this.isAsync || callMetadata.funcTypeIsAsync) && callMetadata.resultPtr && memory) {
				let { resultPtr, realloc } = callMetadata;
				let lowers = callMetadata.lowers;
				lowers && lowers.length > 0 && lowers[0]({
					componentIdx: this.#componentIdx,
					memory,
					realloc,
					vals: [subtaskValue],
					storagePtr: resultPtr,
					stringEncoding: callMetadata.stringEncoding
				});
			}
			this.#resolved = !0;
			this.#parentTask.removeSubtask(this);
			if (!this.isAsync) {
				this.deliverResolve();
				let rep = this.waitableRep();
				if (rep) try {
					if (this.#getComponentState().handles.remove(rep) !== this) throw Error("unexpectedly received non-self Subtask from handle removal");
					this.drop();
				} catch (err) {
					__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#onResolve()] failed to remove subtask after sync subtask completion", err);
				}
			}
		}
		getStateNumber() {
			return this.#state;
		}
		isReturned() {
			return this.#state === 2;
		}
		getCallMetadata() {
			return this.#callMetadata;
		}
		isResolved() {
			return this.#state >= 2;
		}
		addLender(handle) {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#addLender()] args", { handle });
			if (!handle || typeof handle.lends != "number") throw Error("missing/invalid lender handle [" + handle + "]");
			if (this.#lenders === null || this.isResolved()) throw Error("subtask has no lendors or has already been resolved");
			handle.lends++;
			this.#lenders.push(handle);
		}
		deliverResolve() {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#deliverResolve()] args", {
				lenders: this.#lenders,
				parentTaskID: this.parentTaskID(),
				subtaskID: this.#id,
				childTaskID: this.childTaskID(),
				resolved: this.isResolved(),
				resolveDelivered: this.resolveDelivered()
			});
			if (this.resolveDelivered() || !this.isResolved()) throw Error("subtask cannot deliver resolution twice, and the subtask must be resolved");
			for (let lender of this.#lenders) lender.lends--;
			this.#lenders = null;
		}
		resolveDelivered() {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#resolveDelivered()] args", {});
			if (this.#lenders === null && !this.isResolved()) throw Error("invalid subtask state, lenders missing and subtask has not been resolved");
			return this.#lenders === null;
		}
		drop() {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#drop()] args", {
				componentIdx: this.#componentIdx,
				parentTaskID: this.#parentTask?.id(),
				parentTaskFnName: this.#parentTask?.entryFnName(),
				childTaskID: this.#childTask?.id(),
				childTaskFnName: this.#childTask?.entryFnName(),
				subtaskFnName: this.fnName
			});
			if (!this.#waitable) throw Error("missing/invalid inner waitable");
			if (!this.resolveDelivered()) throw new RuntimeError("cannot drop a subtask which has not yet resolved");
			this.#waitable && this.#waitable.drop();
		}
		#getComponentState() {
			let state = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			if (!state) throw Error("invalid/missing async state for component [" + this.#componentIdx + "]");
			return state;
		}
		getWaitableHandleIdx() {
			__jcoEmbeddedRuntime$debugLog("[AsyncSubtask#getWaitableHandleIdx()] args", {});
			if (!this.#waitable) throw Error("missing/invalid waitable");
			return this.waitableRep();
		}
	}
	return AsyncSubtask;
}
function __jcoEmbeddedRuntime$lowerImportBackwardsCompat(_context, args, ...params) {
	__jcoEmbeddedRuntime$debugLog("[_lowerImportBackwardsCompat()] args", {
		args,
		params
	});
	let { componentIdx, isAsync, isManualAsync, resultLowerFns, hasResultPointer, funcTypeIsAsync, memoryIdx, getMemoryFn, getReallocFn, importFn, stringEncoding } = args;
	__jcoEmbeddedRuntime$checkMayLeave(_context.mayLeave, componentIdx);
	let meta = __jcoEmbeddedRuntime$getCurrentTaskMeta(_context.registers, componentIdx);
	let taskMeta = meta && __jcoEmbeddedRuntime$getCurrentTask(_context, componentIdx, meta.taskID);
	let createdTask;
	if (!taskMeta) {
		if (funcTypeIsAsync || isAsync && !isManualAsync) throw Error("p3 async wasm exports cannot use backwards compat auto-task init");
		let { task: newTask, taskID: newTaskID, threadID: newThreadID } = __jcoEmbeddedRuntime$createNewCurrentImplicitThread(_context.creation, {
			componentIdx,
			isAsync,
			isManualAsync,
			callingWasmExport: !1
		});
		createdTask = newTask;
		createdTask.registerOnResolveHandler(() => {
			__jcoEmbeddedRuntime$clearCurrentTaskMeta(_context.registers, {
				taskID: newTaskID,
				threadID: newThreadID,
				componentIdx: task.componentIdx()
			});
		});
		__jcoEmbeddedRuntime$setCurrentTaskMeta(_context.registers, {
			componentIdx,
			taskID: newTaskID,
			threadID: newThreadID
		});
		taskMeta = __jcoEmbeddedRuntime$getCurrentTask(_context, componentIdx, newTaskID);
	}
	if (!taskMeta) throw Error("invalid/missing async task meta");
	let task = taskMeta.task;
	if (!task) throw Error("invalid/missing async task");
	__jcoEmbeddedRuntime$getOrCreateAsyncState(_context.lookup, componentIdx);
	if (!task.mayBlock() && funcTypeIsAsync && !isAsync) throw Error("non async exports cannot synchronously call async functions");
	let memory = getMemoryFn();
	let resultPtr = hasResultPointer ? params[params.length - 1] >>> 0 : void 0;
	let subtask = task.createSubtask({
		componentIdx,
		parentTask: task,
		fnName: importFn.fnName,
		isAsync,
		isManualAsync,
		callMetadata: {
			memoryIdx,
			memory,
			realloc: getReallocFn?.(),
			getReallocFn,
			resultPtr,
			lowers: resultLowerFns,
			funcTypeIsAsync,
			stringEncoding
		}
	});
	task.setReturnMemoryIdx(memoryIdx);
	task.setReturnMemory(getMemoryFn());
	subtask.onStart();
	if (!isManualAsync && !isAsync && !funcTypeIsAsync) {
		createdTask && createdTask.enterSync();
		importFn(...params);
		if (!funcTypeIsAsync && !subtask.isReturned()) throw Error("post-execution subtasks must either be async or returned");
		let syncRes = subtask.getResult();
		if (createdTask) {
			createdTask.resolve([syncRes]);
			createdTask.exit();
		}
		return syncRes;
	}
	if (!isManualAsync && !isAsync && funcTypeIsAsync) {
		let { promise, resolve, reject } = __jcoEmbeddedRuntime$promiseWithResolvers();
		queueMicrotask(async () => {
			try {
				await importFn(...params);
				subtask.isResolved() || await task.suspendUntil({ readyFn: () => subtask.isResolved() });
				resolve(subtask.getResult());
			} catch (err) {
				reject(err);
			}
		});
		return promise;
	}
	let subtaskState = subtask.getStateNumber();
	if (subtaskState < 0 || subtaskState >= 16) throw Error("invalid subtask state, out of valid range");
	subtask.setOnProgressFn(() => {
		subtask.setPendingEvent(() => {
			subtask.isResolved() && subtask.deliverResolve();
			return {
				code: _context.events.SUBTASK,
				payload0: subtask.waitableRep(),
				payload1: subtask.getStateNumber()
			};
		});
	});
	if (!isAsync && !funcTypeIsAsync && isManualAsync) {
		__jcoEmbeddedRuntime$debugLog("[_lowerImportBackwardsCompat()] calling lowered import", {
			importFn,
			params
		});
		createdTask && createdTask.enterSync();
		try {
			let result = importFn(...params);
			let isPromise = __jcoEmbeddedRuntime$isPromiseLike(result);
			createdTask && createdTask.resolve([isPromise ? void 0 : result]);
			if (!isPromise) return subtask.getResult();
			return Promise.resolve(result).then(() => subtask.getResult());
		} catch (error) {
			createdTask && createdTask.reject(error);
			throw error;
		} finally {
			createdTask && createdTask.exit();
		}
	}
	queueMicrotask(async () => {
		try {
			__jcoEmbeddedRuntime$debugLog("[_lowerImportBackwardsCompat()] calling lowered import", {
				importFn,
				params
			});
			createdTask && await createdTask.enter();
			let asyncRes = await importFn(...params);
			createdTask && createdTask.resolve([asyncRes]);
		} catch (err) {
			__jcoEmbeddedRuntime$debugLog("[_lowerImportBackwardsCompat()] import fn error:", err);
			task.setErrored(err);
			task.reject(err);
		}
	});
	__jcoEmbeddedRuntime$debugLog("[_lowerImportBackwardsCompat()] async-lowered import return", {
		fnName: importFn.fnName,
		componentIdx,
		subtaskID: subtask.id(),
		waitableRep: subtask.waitableRep(),
		subtaskState,
		packedResult: Number(subtask.waitableRep()) << 4 | subtaskState
	});
	return Number(subtask.ensureHandle()) << 4 | subtaskState;
}
const __jcoEmbeddedRuntime$THREAD_NOT_STARTED = "not-started";
const __jcoEmbeddedRuntime$THREAD_RUNNING = "running";
const __jcoEmbeddedRuntime$THREAD_COMPLETED = "completed";
function __jcoEmbeddedRuntime$initializeAsyncThreadClass(context) {
	let { lookup: _lookup, registers: _registers } = context;
	class AsyncThread {
		static State = {
			NOT_STARTED: __jcoEmbeddedRuntime$THREAD_NOT_STARTED,
			RUNNING: __jcoEmbeddedRuntime$THREAD_RUNNING,
			COMPLETED: __jcoEmbeddedRuntime$THREAD_COMPLETED
		};
		#id = null;
		#componentIdx;
		#task;
		#state;
		#startFn = null;
		storage = [0, 0];
		constructor(opts) {
			if (opts?.componentIdx === void 0) throw TypeError("missing component id during thread creation");
			this.#componentIdx = opts.componentIdx;
			if (!opts?.task) throw TypeError("missing task during thread creation");
			this.#task = opts.task;
			if (opts.startFn) {
				this.#startFn = opts.startFn;
				this.#state = __jcoEmbeddedRuntime$THREAD_NOT_STARTED;
			} else this.#state = __jcoEmbeddedRuntime$THREAD_RUNNING;
		}
		id() {
			return this.#id;
		}
		setId(id) {
			if (this.#id !== null) throw Error("thread id can only be assigned once");
			this.#id = id;
		}
		componentIdx() {
			return this.#componentIdx;
		}
		task() {
			return this.#task;
		}
		taskID() {
			return this.#task.id();
		}
		state() {
			return this.#state;
		}
		isCancelled() {
			return this.#task.isCancelled();
		}
		isRejected() {
			return this.#task.isRejected();
		}
		notifyProgress() {
			return this.#task.notifyProgress();
		}
		entryFnName() {
			return this.#task.entryFnName();
		}
		getParentSubtask() {
			return this.#task.getParentSubtask();
		}
		async suspend(opts) {
			let cstate = __jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx);
			let task = this.#task;
			task.endFirstSlice();
			task.settleCompletion();
			let { cancellable, readyFn, releaseLock = !1 } = opts;
			if (task.deliverPendingCancel({ cancellable })) return !1;
			let reacquireLock = releaseLock && task.needsExclusiveLock() && cstate.exclusivelyLockedBy(task.id());
			reacquireLock && cstate.exclusiveRelease(task.id());
			try {
				let keepGoing = await cstate.suspendThread({
					...opts,
					cancellable: !!cancellable,
					thread: this,
					readyFn: () => cancellable && task.cancellationPending() || readyFn()
				});
				cstate.throwIfInterrupted();
				if (keepGoing && task.deliverPendingCancel({ cancellable })) return !1;
				return !!keepGoing;
			} finally {
				reacquireLock && !cstate.exclusivelyLockedBy(task.id()) && await cstate.acquireExclusiveLock(task.id());
			}
		}
		start() {
			if (this.#state !== __jcoEmbeddedRuntime$THREAD_NOT_STARTED) throw Error(`cannot start thread [${this.#id}] in state [${this.#state}]`);
			this.#state = __jcoEmbeddedRuntime$THREAD_RUNNING;
			let startFn = this.#startFn;
			this.#startFn = null;
			__jcoEmbeddedRuntime$withCurrentTaskMetaAsync(_registers, {
				threadID: this.#id,
				taskID: this.#task.id(),
				componentIdx: this.#componentIdx,
				fn: async () => {
					__jcoEmbeddedRuntime$getOrCreateAsyncState(_lookup, this.#componentIdx).throwIfInterrupted();
					return startFn();
				}
			}).then(() => {
				this.#state = __jcoEmbeddedRuntime$THREAD_COMPLETED;
				this.#task.removeThread(this);
			}).catch((err) => {
				err = __jcoEmbeddedRuntime$recordTrap(context.traps, this.#componentIdx, err);
				this.#state = __jcoEmbeddedRuntime$THREAD_COMPLETED;
				__jcoEmbeddedRuntime$debugLog("[AsyncThread#start()] spawned thread errored", {
					threadID: this.#id,
					taskID: this.#task.id(),
					componentIdx: this.#componentIdx,
					err
				});
				this.#task.setErrored(err);
				this.#task.reject(err);
				this.#task.removeThread(this);
			});
		}
	}
	return AsyncThread;
}
const __jcoEmbeddedRuntime$initializeStreams = (RepTable) => new RepTable({ target: "global stream map" });
const __jcoEmbeddedRuntime$initializeFutures = (RepTable) => new RepTable({ target: "global future map" });
const __jcoEmbeddedRuntime$decodeUtf8 = (RuntimeError, decoder, input) => {
	try {
		return decoder.decode(input);
	} catch (error) {
		if (!(error instanceof TypeError)) throw error;
		let bytes;
		try {
			bytes = ArrayBuffer.isView(input) ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength) : new Uint8Array(input);
		} catch {
			throw error;
		}
		for (let i = 0; i < bytes.length; i++) {
			let lead = bytes[i];
			if (lead < 128) continue;
			let count = lead >= 194 && lead <= 223 ? 1 : lead >= 224 && lead <= 239 ? 2 : lead >= 240 && lead <= 244 ? 3 : -1;
			if (count < 0) throw new RuntimeError("invalid utf-8");
			for (let j = 1; j <= count; j++) {
				if (i + j >= bytes.length) throw new RuntimeError("incomplete utf-8 byte sequence");
				let byte = bytes[i + j];
				if (byte < 128 || byte > 191 || j === 1 && (lead === 224 && byte < 160 || lead === 237 && byte > 159 || lead === 240 && byte < 144 || lead === 244 && byte > 143)) throw new RuntimeError("invalid utf-8");
			}
			i += count;
		}
		throw error;
	}
};
const __jcoEmbeddedRuntime$initializeLiftVariant = () => ({ scratch: new DataView(new ArrayBuffer(8)) });
function __jcoEmbeddedRuntime$liftFlatBool(ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatBool()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length === 0) throw Error("expected at least a single i32 argument");
		val = ctx.params[0] === 1;
		ctx.params = ctx.params.slice(1);
		return [val, ctx];
	}
	if (ctx.storageLen !== void 0 && ctx.storageLen < 1) throw Error(`insufficient storage ([${ctx.storageLen}] bytes) for lift (bool requires 1 byte)`);
	val = new DataView(ctx.memory.buffer).getUint8(ctx.storagePtr) === 1;
	ctx.storagePtr += 1;
	ctx.storageLen !== void 0 && --ctx.storageLen;
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatU8(ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatU8()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length === 0) throw Error("expected at least a single i32 argument");
		val = ctx.params[0];
		ctx.params = ctx.params.slice(1);
		return [val, ctx];
	}
	if (ctx.storageLen !== void 0 && ctx.storageLen < 1) throw Error(`insufficient storage ([${ctx.storageLen}] bytes) for lift (u8 requires 1 byte)`);
	val = new DataView(ctx.memory.buffer).getUint8(ctx.storagePtr);
	ctx.storagePtr += 1;
	ctx.storageLen !== void 0 && --ctx.storageLen;
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatU16(ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatU16()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length === 0) throw Error("expected at least a single i32 argument");
		val = ctx.params[0];
		ctx.params = ctx.params.slice(1);
		return [val, ctx];
	}
	if (ctx.storageLen !== void 0 && ctx.storageLen < 2) throw Error(`insufficient storage ([${ctx.storageLen}] bytes) for lift (u16 requires 2 bytes)`);
	val = new DataView(ctx.memory.buffer).getUint16(ctx.storagePtr, !0);
	ctx.storagePtr += 2;
	ctx.storageLen !== void 0 && (ctx.storageLen -= 2);
	let rem = ctx.storagePtr % 2;
	rem !== 0 && (ctx.storagePtr += 2 - rem);
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatU32(ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatU32()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length === 0) throw Error("expected at least a single i34 argument");
		val = ctx.params[0] >>> 0;
		ctx.params = ctx.params.slice(1);
		return [val, ctx];
	}
	if (ctx.storageLen !== void 0 && ctx.storageLen < 4) throw Error(`insufficient storage ([${ctx.storageLen}] bytes) for lift (u32 requires 4 bytes)`);
	val = new DataView(ctx.memory.buffer).getUint32(ctx.storagePtr, !0);
	ctx.storagePtr += 4;
	ctx.storageLen !== void 0 && (ctx.storageLen -= 4);
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatU64(ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatU64()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length === 0) throw Error("expected at least one single i64 argument");
		if (typeof ctx.params[0] != "bigint") throw Error("expected bigint");
		val = BigInt.asUintN(64, ctx.params[0]);
		ctx.params = ctx.params.slice(1);
		return [val, ctx];
	}
	if (ctx.storageLen !== void 0 && ctx.storageLen < 8) throw Error(`insufficient storage ([${ctx.storageLen}] bytes) for lift (u64 requires 8 bytes)`);
	val = new DataView(ctx.memory.buffer).getBigUint64(ctx.storagePtr, !0);
	ctx.storagePtr += 8;
	ctx.storageLen !== void 0 && (ctx.storageLen -= 8);
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatStringAny(_context, ctx) {
	switch (ctx.stringEncoding) {
		case "utf8": return __jcoEmbeddedRuntime$liftFlatStringUTF8(_context, ctx);
		case "utf16": return __jcoEmbeddedRuntime$liftFlatStringUTF16(_context, ctx);
		case "compact-utf16": {
			let [ptr, ptrCtx] = __jcoEmbeddedRuntime$liftFlatU32(ctx);
			let [len, newCtx] = __jcoEmbeddedRuntime$liftFlatU32(ptrCtx);
			return [__jcoEmbeddedRuntime$compactUtf16Decode(_context.utf16, ctx.memory, ptr, len), newCtx];
		}
		default: throw Error(`missing/unrecognized/unsupported string encoding [${ctx.stringEncoding}]`);
	}
}
function __jcoEmbeddedRuntime$requireStringInMemory(RuntimeError, memory, offset, codeUnits, unitSize) {
	if (offset % unitSize !== 0) throw new RuntimeError("wasm trap: unaligned pointer");
	let byteLength = memory.buffer.byteLength;
	if (offset > byteLength || codeUnits < 0 || codeUnits > Math.floor((byteLength - offset) / unitSize)) throw new RuntimeError("string pointer/length out of bounds of memory");
}
function __jcoEmbeddedRuntime$liftFlatStringUTF8(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatStringUTF8()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length < 2) throw Error("expected at least two u32 arguments");
		let offset = ctx.params[0];
		typeof offset == "bigint" ? offset = Number(offset) : offset >>>= 0;
		if (!Number.isSafeInteger(offset)) throw Error("invalid offset");
		let len = ctx.params[1];
		if (!Number.isSafeInteger(len)) throw Error("invalid len");
		__jcoEmbeddedRuntime$requireStringInMemory(_context.RuntimeError, ctx.memory, offset, len, 1);
		val = __jcoEmbeddedRuntime$decodeUtf8(_context.RuntimeError, _context.utf8, new DataView(ctx.memory.buffer, offset, len));
		ctx.params = ctx.params.slice(2);
		return [val, ctx];
	}
	let rem = ctx.storagePtr % 4;
	rem !== 0 && (ctx.storagePtr += 4 - rem);
	let dv = new DataView(ctx.memory.buffer);
	let start = dv.getUint32(ctx.storagePtr, !0);
	let codeUnits = dv.getUint32(ctx.storagePtr + 4, !0);
	__jcoEmbeddedRuntime$requireStringInMemory(_context.RuntimeError, ctx.memory, start, codeUnits, 1);
	val = __jcoEmbeddedRuntime$decodeUtf8(_context.RuntimeError, _context.utf8, new Uint8Array(ctx.memory.buffer, start, codeUnits));
	ctx.storagePtr += 8;
	ctx.storageLen !== void 0 && (ctx.storageLen -= 8);
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatStringUTF16(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_liftFlatStringUTF16()] args", { ctx });
	let val;
	if (ctx.useDirectParams) {
		if (ctx.params.length < 2) throw Error("expected at least two u32 arguments");
		let offset = ctx.params[0];
		typeof offset == "bigint" ? offset = Number(offset) : offset >>>= 0;
		if (!Number.isSafeInteger(offset)) throw Error("invalid offset");
		let len = ctx.params[1];
		if (!Number.isSafeInteger(len)) throw Error("invalid len");
		__jcoEmbeddedRuntime$requireStringInMemory(_context.RuntimeError, ctx.memory, offset, len, 2);
		val = _context.utf16.decode(new DataView(ctx.memory.buffer, offset, len * 2));
		ctx.params = ctx.params.slice(2);
		return [val, ctx];
	}
	let rem = ctx.storagePtr % 4;
	rem !== 0 && (ctx.storagePtr += 4 - rem);
	let dv = new DataView(ctx.memory.buffer);
	let start = dv.getUint32(ctx.storagePtr, !0);
	let codeUnits = dv.getUint32(ctx.storagePtr + 4, !0);
	__jcoEmbeddedRuntime$requireStringInMemory(_context.RuntimeError, ctx.memory, start, codeUnits, 2);
	val = _context.utf16.decode(new DataView(ctx.memory.buffer, start, codeUnits * 2));
	ctx.storagePtr += 8;
	ctx.storageLen !== void 0 && (ctx.storageLen -= 8);
	return [val, ctx];
}
function __jcoEmbeddedRuntime$liftFlatRecord(meta) {
	let { fieldMetas, size32: recordSize32, align32: recordAlign32 } = meta;
	return function liftFlatRecordInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_liftFlatRecord()] args", { ctx });
		let originalPtr = ctx.storagePtr;
		let res = {};
		for (let [key, liftFn, size32, align32] of fieldMetas) {
			let fieldPtr;
			if (ctx.storagePtr !== void 0) {
				let rem = ctx.storagePtr % align32;
				rem !== 0 && (ctx.storagePtr += align32 - rem);
				fieldPtr = ctx.storagePtr;
			}
			let fieldLen;
			ctx.storageLen !== void 0 && (fieldLen = ctx.storageLen);
			let [val, newCtx] = liftFn(ctx);
			res[key] = val;
			ctx = newCtx;
			fieldPtr !== void 0 && (ctx.storagePtr = Math.max(ctx.storagePtr, fieldPtr + size32));
			fieldLen !== void 0 && (ctx.storageLen = fieldLen - size32);
		}
		originalPtr !== void 0 && (ctx.storagePtr = Math.max(ctx.storagePtr, originalPtr + recordSize32));
		if (ctx.storagePtr !== void 0) {
			let rem = ctx.storagePtr % recordAlign32;
			rem !== 0 && (ctx.storagePtr += recordAlign32 - rem);
		}
		return [res, ctx];
	};
}
function __jcoEmbeddedRuntime$liftFlatVariant(_context, meta) {
	let { caseMetas, variantSize32, variantAlign32, variantPayloadOffset32, variantFlatCount, variantPayloadFlatTypes, isEnum } = meta;
	return function liftFlatVariantInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_liftFlatVariant()] args", { ctx });
		let origUseParams = ctx.useDirectParams;
		let caseIdx;
		let liftRes;
		let originalPtr = ctx.storagePtr;
		let numCases = caseMetas.length;
		if (caseMetas.length < 256) liftRes = __jcoEmbeddedRuntime$liftFlatU8(ctx);
		else if (numCases >= 256 && numCases < 65536) liftRes = __jcoEmbeddedRuntime$liftFlatU16(ctx);
		else if (numCases >= 65536 && numCases < 4294967296) liftRes = __jcoEmbeddedRuntime$liftFlatU32(ctx);
		else throw Error(`unsupported number of variant cases [${numCases}]`);
		caseIdx = liftRes[0];
		ctx = liftRes[1];
		let [tag, liftFn, , , caseFlatCount, caseFlatTypes] = caseMetas[caseIdx];
		if (variantPayloadOffset32 === void 0) throw Error("unexpectedly missing payload offset");
		originalPtr !== void 0 && (ctx.storagePtr = originalPtr + variantPayloadOffset32);
		let val;
		if (liftFn === null) {
			val = { tag };
			originalPtr !== void 0 && (ctx.storagePtr = originalPtr + variantSize32);
		} else {
			if (ctx.useDirectParams) {
				if (!variantPayloadFlatTypes || !caseFlatTypes) throw Error("missing variant flat type metadata during direct-param lift");
				let scratch = _context.scratch;
				for (let i = 0; i < caseFlatTypes.length; i++) {
					let have = variantPayloadFlatTypes[i];
					let want = caseFlatTypes[i];
					if (have === want) continue;
					let val = ctx.params[i];
					if (have === "i64" && want === "i32") ctx.params[i] = Number(BigInt.asIntN(32, val));
					else if (have === "i64" && want === "f32") {
						scratch.setInt32(0, Number(BigInt.asIntN(32, val)), !0);
						ctx.params[i] = scratch.getFloat32(0, !0);
					} else if (have === "i64" && want === "f64") {
						scratch.setBigInt64(0, val, !0);
						ctx.params[i] = scratch.getFloat64(0, !0);
					} else if (have === "i32" && want === "f32") {
						scratch.setInt32(0, val, !0);
						ctx.params[i] = scratch.getFloat32(0, !0);
					} else throw Error(`invalid variant payload coercion [${have}] -> [${want}]`);
				}
			}
			let [newVal, newCtx] = liftFn(ctx);
			val = {
				tag,
				val: newVal
			};
			ctx = newCtx;
		}
		if (origUseParams) {
			if (variantFlatCount == null) {
				__jcoEmbeddedRuntime$debugLog("[_liftFlatVariant()] variant with unknown flat count", {
					ctx,
					meta
				});
				throw Error("cannot lift variant with unknown flat count");
			}
			if (caseFlatCount == null) {
				__jcoEmbeddedRuntime$debugLog("[_liftFlatVariant()] case with unknown flat count", {
					ctx,
					meta,
					case: meta.caseMetas[caseIdx]
				});
				throw Error("cannot lift case with unknown flat count");
			}
			let remainingPayloadParams = variantFlatCount - caseFlatCount - +!isEnum;
			if (remainingPayloadParams < 0) throw Error("invalid variant flat count metadata");
			if (ctx.params.length < remainingPayloadParams) throw Error(`expected at least [${remainingPayloadParams}] remaining variant payload params, but got [${ctx.params.length}]`);
			ctx.params = ctx.params.slice(remainingPayloadParams);
		}
		if (ctx.storagePtr !== void 0) {
			let rem = ctx.storagePtr % variantAlign32;
			rem !== 0 && (ctx.storagePtr += variantAlign32 - rem);
		}
		return [val, ctx];
	};
}
function __jcoEmbeddedRuntime$liftFlatList(_context, meta) {
	let { elemLiftFn, elemSize32, elemAlign32, knownLen, typedArray } = meta;
	let listValue = typedArray === void 0 ? (values) => values : (values) => new typedArray(values);
	let readValuesAndReset = (ctx, originalPtr, originalLen, dataPtr, len) => {
		if (dataPtr < 0 || len < 0 || elemSize32 !== 0 && len > Math.floor(((1 << 28) - 1) / elemSize32) || dataPtr > ctx.memory.buffer.byteLength || elemSize32 !== 0 && len > Math.floor((ctx.memory.buffer.byteLength - dataPtr) / elemSize32)) throw new _context.RuntimeError("wasm trap: out of bounds memory access");
		if (dataPtr % elemAlign32 !== 0) throw TypeError(`list pointer [${dataPtr}] is not aligned to ${elemAlign32}`);
		ctx.storagePtr = dataPtr;
		let val;
		if (typedArray !== void 0 && (_context.littleEndian || elemSize32 === 1)) {
			ctx.storagePtr = dataPtr + len * elemSize32;
			val = new typedArray(ctx.memory.buffer.slice(dataPtr, ctx.storagePtr));
		} else {
			let values = [];
			for (let i = 0; i < len; i++) {
				let elemPtr = dataPtr + i * elemSize32;
				ctx.storagePtr = elemPtr;
				let [res, nextCtx] = elemLiftFn(ctx);
				values.push(res);
				ctx = nextCtx;
				ctx.storagePtr = Math.max(ctx.storagePtr, elemPtr + elemSize32);
			}
			val = listValue(values);
		}
		originalPtr !== null && (ctx.storagePtr = originalPtr);
		originalLen !== null && (ctx.storageLen = originalLen);
		return [val, ctx];
	};
	return function liftFlatListInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_liftFlatList()] args", { ctx });
		let liftResults;
		if (knownLen !== void 0) {
			if (ctx.useDirectParams) {
				__jcoEmbeddedRuntime$debugLog("memory unexpectedly missing while lifting unknown length list", { ctx });
				liftResults = [listValue(ctx.params.slice(0, knownLen)), ctx];
				ctx.params = ctx.params.slice(knownLen);
			} else {
				if (ctx.memory === null) {
					__jcoEmbeddedRuntime$debugLog("memory unexpectedly missing while lifting known length list", {
						knownLen,
						ctx
					});
					throw Error(`memory missing while lifting known length (${knownLen}) list`);
				}
				let originalLen = ctx.storageLen;
				ctx.storageLen = knownLen * elemSize32;
				liftResults = readValuesAndReset(ctx, null, originalLen, ctx.storagePtr, knownLen);
			}
		} else if (ctx.useDirectParams) {
			let dataPtr = ctx.params[0] >>> 0;
			let len = ctx.params[1];
			ctx.params = ctx.params.slice(2);
			ctx.useDirectParams = !1;
			let originalPtr = ctx.storagePtr;
			let originalLen = ctx.storageLen;
			ctx.storageLen = len * elemSize32;
			liftResults = readValuesAndReset(ctx, originalPtr, originalLen, dataPtr, len);
			ctx.useDirectParams = !0;
		} else {
			let originalLen = ctx.storageLen;
			ctx.storageLen = 8;
			let dataPtrLiftRes = __jcoEmbeddedRuntime$liftFlatU32(ctx);
			let dataPtr = dataPtrLiftRes[0];
			ctx = dataPtrLiftRes[1];
			let lenLiftRes = __jcoEmbeddedRuntime$liftFlatU32(ctx);
			let len = lenLiftRes[0];
			ctx = lenLiftRes[1];
			let originalPtr = ctx.storagePtr;
			ctx.storagePtr = dataPtr;
			ctx.storageLen = len * elemSize32;
			liftResults = readValuesAndReset(ctx, originalPtr, originalLen, dataPtr, len);
		}
		return liftResults;
	};
}
function __jcoEmbeddedRuntime$liftFlatOption(_context, meta) {
	let { payloadMaybeNull } = meta;
	let f = __jcoEmbeddedRuntime$liftFlatVariant(_context, meta);
	return function liftFlatOptionInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_liftFlatOption()] args", { ctx });
		let res = f(ctx);
		if (!payloadMaybeNull) {
			let value = res[0];
			res[0] = value.tag === "none" ? void 0 : value.val;
		}
		return res;
	};
}
function __jcoEmbeddedRuntime$lowerFlatBool(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatBool()] args", { ctx });
	if (!ctx.memory) throw Error("missing memory for lower");
	if (ctx.vals.length !== 1) throw Error(`unexpected number [${ctx.vals.length}] of vals (expected 1)`);
	if (_context.strict && typeof ctx.vals[0] != "boolean") throw TypeError(`invalid bool value [${ctx.vals[0]}]`);
	new DataView(ctx.memory.buffer).setUint8(ctx.storagePtr, +!!ctx.vals[0]);
	ctx.storagePtr += 1;
}
function __jcoEmbeddedRuntime$lowerFlatS8(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatS8()] args", { ctx });
	if (ctx.vals.length !== 1) throw Error(`unexpected number [${ctx.vals.length}] of vals (expected 1)`);
	if (!ctx.memory) throw Error("missing memory for lower");
	_context.strict && !__jcoEmbeddedRuntime$isInt8(ctx.vals[0]) && __jcoEmbeddedRuntime$invalidNumericPrimitive("s8", ctx.vals[0]);
	new DataView(ctx.memory.buffer).setInt8(ctx.storagePtr, ctx.vals[0]);
	ctx.storagePtr += 1;
}
function __jcoEmbeddedRuntime$lowerFlatU8(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatU8()] args", ctx);
	if (ctx.vals.length !== 1) throw Error(`unexpected number [${ctx.vals.length}] of vals (expected 1)`);
	_context.strict && !__jcoEmbeddedRuntime$isUint8(ctx.vals[0]) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u8", ctx.vals[0]);
	if (!ctx.memory) throw Error("missing memory for lower");
	new DataView(ctx.memory.buffer).setUint8(ctx.storagePtr, ctx.vals[0]);
	ctx.storagePtr += 1;
}
function __jcoEmbeddedRuntime$lowerFlatU16(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatU16()] args", { ctx });
	if (!ctx.memory) throw Error("missing memory for lower");
	if (ctx.vals.length !== 1) throw Error(`unexpected number [${ctx.vals.length}] of vals (expected 1)`);
	let rem = ctx.storagePtr % 2;
	rem !== 0 && (ctx.storagePtr += 2 - rem);
	_context.strict && !__jcoEmbeddedRuntime$isUint16(ctx.vals[0]) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u16", ctx.vals[0]);
	new DataView(ctx.memory.buffer).setUint16(ctx.storagePtr, ctx.vals[0], !0);
	ctx.storagePtr += 2;
}
function __jcoEmbeddedRuntime$lowerFlatU32(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatU32()] args", { ctx });
	if (ctx.vals.length !== 1) throw Error(`expected single value to lower, got [${ctx.vals.length}]`);
	let rem = ctx.storagePtr % 4;
	rem !== 0 && (ctx.storagePtr += 4 - rem);
	_context.strict && !__jcoEmbeddedRuntime$isUint32(ctx.vals[0]) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u32", ctx.vals[0]);
	new DataView(ctx.memory.buffer).setUint32(ctx.storagePtr, ctx.vals[0], !0);
	ctx.storagePtr += 4;
}
function __jcoEmbeddedRuntime$lowerFlatU64(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatU64()] args", { ctx });
	if (ctx.vals.length !== 1) throw Error("unexpected number of vals");
	let rem = ctx.storagePtr % 8;
	rem !== 0 && (ctx.storagePtr += 8 - rem);
	_context.strict && !__jcoEmbeddedRuntime$isUint64(ctx.vals[0]) && __jcoEmbeddedRuntime$invalidNumericPrimitive("u64", ctx.vals[0]);
	new DataView(ctx.memory.buffer).setBigUint64(ctx.storagePtr, ctx.vals[0], !0);
	ctx.storagePtr += 8;
}
function __jcoEmbeddedRuntime$lowerFlatStringAny(_context, ctx) {
	switch (ctx.stringEncoding) {
		case "utf8": return __jcoEmbeddedRuntime$lowerFlatStringUTF8(_context.utf8, ctx);
		case "utf16": return __jcoEmbeddedRuntime$lowerFlatStringUTF16(_context.utf16, ctx);
		case "compact-utf16": {
			if (!ctx.realloc) throw Error("missing realloc during flat string lower");
			let { ptr, len } = __jcoEmbeddedRuntime$compactUtf16Encode(_context.utf16, ctx.vals[0], ctx.realloc, ctx.memory);
			let view = new DataView(ctx.memory.buffer);
			view.setUint32(ctx.storagePtr, ptr, !0);
			view.setUint32(ctx.storagePtr + 4, len, !0);
			ctx.storagePtr += 8;
			return;
		}
		default: throw Error(`missing/unrecognized/unsupported string encoding [${ctx.stringEncoding}]`);
	}
}
function __jcoEmbeddedRuntime$lowerFlatStringUTF16(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatStringUTF16()] args", { ctx });
	if (!ctx.realloc) throw Error("missing realloc during flat string lower");
	let { ptr, len } = __jcoEmbeddedRuntime$utf16Encode(_context, ctx.vals[0], ctx.realloc, ctx.memory);
	let view = new DataView(ctx.memory.buffer);
	view.setUint32(ctx.storagePtr, ptr, !0);
	view.setUint32(ctx.storagePtr + 4, len, !0);
	ctx.storagePtr += 8;
}
function __jcoEmbeddedRuntime$lowerFlatStringUTF8(_context, ctx) {
	__jcoEmbeddedRuntime$debugLog("[_lowerFlatStringUTF8()] args", ctx);
	if (!ctx.realloc) throw Error("missing realloc during flat string lower");
	let { ptr, len } = __jcoEmbeddedRuntime$utf8Encode(_context, ctx.vals[0], ctx.realloc, ctx.memory);
	let view = new DataView(ctx.memory.buffer);
	view.setUint32(ctx.storagePtr, ptr, !0);
	view.setUint32(ctx.storagePtr + 4, len, !0);
	ctx.storagePtr += 8;
}
function __jcoEmbeddedRuntime$lowerFlatRecord(meta) {
	let { fieldMetas, size32: recordSize32, align32: recordAlign32 } = meta;
	return function lowerFlatRecordInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_lowerFlatRecord()] args", { ctx });
		let originalPtr = ctx.storagePtr;
		let r = ctx.vals[0];
		for (let [tag, lowerFn, size32, align32] of fieldMetas) {
			let rem = ctx.storagePtr % align32;
			rem !== 0 && (ctx.storagePtr += align32 - rem);
			let fieldPtr = ctx.storagePtr;
			ctx.vals = [r[tag]];
			lowerFn(ctx);
			ctx.storagePtr = Math.max(ctx.storagePtr, fieldPtr + size32);
		}
		ctx.storagePtr = Math.max(ctx.storagePtr, originalPtr + recordSize32);
		let rem = ctx.storagePtr % recordAlign32;
		rem !== 0 && (ctx.storagePtr += recordAlign32 - rem);
	};
}
function __jcoEmbeddedRuntime$lowerFlatVariant(_context, meta) {
	let { variantSize32, variantAlign32, variantPayloadOffset32, caseMetas } = meta;
	let caseLookup = Object.create(null);
	for (let [idx, meta] of caseMetas.entries()) {
		let tag = meta[0];
		caseLookup[tag] = {
			discriminant: idx,
			meta
		};
	}
	return function lowerFlatVariantInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_lowerFlatVariant()] args", { ctx });
		let { tag, val } = ctx.vals[0];
		let variantCase = caseLookup[tag];
		if (!variantCase) throw Error(`missing tag [${tag}] (valid tags: ${Object.keys(caseLookup)})`);
		let [, lowerFn] = variantCase.meta;
		let originalPtr = ctx.storagePtr;
		ctx.vals = [variantCase.discriminant];
		if (caseMetas.length < 256) __jcoEmbeddedRuntime$lowerFlatU8(_context, ctx);
		else if (caseMetas.length >= 256 && caseMetas.length < 65536) __jcoEmbeddedRuntime$lowerFlatU16(_context, ctx);
		else if (caseMetas.length >= 65536 && caseMetas.length < 4294967296) __jcoEmbeddedRuntime$lowerFlatU32(_context, ctx);
		else throw Error(`unsupported number of cases [${caseMetas.length}]`);
		ctx.storagePtr = originalPtr + variantPayloadOffset32;
		ctx.vals = [val];
		lowerFn && lowerFn(ctx);
		ctx.storagePtr = Math.max(ctx.storagePtr, originalPtr + variantSize32);
		let rem = ctx.storagePtr % variantAlign32;
		rem !== 0 && (ctx.storagePtr += variantAlign32 - rem);
	};
}
function __jcoEmbeddedRuntime$lowerFlatList(_context, meta) {
	let { elemLowerFn, knownLen, size32, elemSize32, elemAlign32, typedArray } = meta;
	if (!elemLowerFn) throw TypeError("missing/invalid element lower fn for list");
	return function lowerFlatListInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_lowerFlatList()] args", { ctx });
		if (ctx.useDirectParams) {
			if (ctx.params.length < 2) throw Error("insufficient params left to lower list");
			let storagePtr = ctx.params[0] >>> 0;
			let elemCount = ctx.params[1];
			ctx.params = ctx.params.slice(2);
			let list = ctx.vals[0];
			if (!list) throw Error("missing direct param value");
			if (elemCount !== list.length) throw Error("list length does not match allocated storage");
			let lowerCtx = {
				storagePtr,
				memory: ctx.memory,
				stringEncoding: ctx.stringEncoding,
				componentIdx: ctx.componentIdx,
				realloc: ctx.realloc,
				lowerResource: ctx.lowerResource,
				vals: []
			};
			for (let idx = 0; idx < list.length; idx++) {
				let elemPtr = storagePtr + idx * elemSize32;
				lowerCtx.storagePtr = elemPtr;
				lowerCtx.vals = list.slice(idx, idx + 1);
				elemLowerFn(lowerCtx);
				lowerCtx.storagePtr = Math.max(lowerCtx.storagePtr, elemPtr + elemSize32);
			}
			ctx.storagePtr = lowerCtx.storagePtr;
			return;
		}
		let elems = ctx.vals[0];
		let totalSizeBytes = knownLen === void 0 ? 8 : size32 ?? elems.length * elemSize32;
		if (ctx.storageLen !== void 0 && totalSizeBytes > ctx.storageLen) throw Error("not enough storage remaining for list flat lower");
		if (knownLen === void 0) {
			if (!ctx.realloc) throw Error("missing realloc during flat string lower");
			let dataPtr = ctx.realloc(0, 0, elemAlign32, elemSize32 * elems.length);
			ctx.vals[0] = dataPtr;
			__jcoEmbeddedRuntime$lowerFlatU32(_context.numeric, ctx);
			ctx.vals[0] = elems.length;
			__jcoEmbeddedRuntime$lowerFlatU32(_context.numeric, ctx);
			let origPtr = ctx.storagePtr;
			ctx.storagePtr = dataPtr;
			if (typedArray !== void 0 && elems instanceof typedArray && (_context.littleEndian || elemSize32 === 1)) new Uint8Array(ctx.memory.buffer, dataPtr, elems.byteLength).set(new Uint8Array(elems.buffer, elems.byteOffset, elems.byteLength));
			else for (let [idx, elem] of elems.entries()) {
				let elemPtr = dataPtr + idx * elemSize32;
				ctx.storagePtr = elemPtr;
				ctx.vals = [elem];
				elemLowerFn(ctx);
				ctx.storagePtr = Math.max(ctx.storagePtr, elemPtr + elemSize32);
			}
			ctx.storagePtr = origPtr;
		} else {
			if (elems.length !== knownLen) throw TypeError(`invalid list input of length [${elems.length}], must be length [${knownLen}]`);
			let originalPtr = ctx.storagePtr;
			for (let [idx, elem] of elems.entries()) {
				let elemPtr = originalPtr + idx * elemSize32;
				ctx.storagePtr = elemPtr;
				ctx.vals = [elem];
				elemLowerFn(ctx);
				ctx.storagePtr = Math.max(ctx.storagePtr, elemPtr + elemSize32);
			}
		}
	};
}
function __jcoEmbeddedRuntime$lowerFlatOption(_context, meta) {
	let { payloadMaybeNull } = meta;
	let f = __jcoEmbeddedRuntime$lowerFlatVariant(_context, meta);
	return function lowerFlatOptionInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_lowerFlatOption()] args", { ctx });
		let v = ctx.vals[0];
		v == null ? ctx.vals[0] = { tag: "none" } : payloadMaybeNull ? (typeof v != "object" || Object.keys(v).length !== 2 || !("tag" in v) || v.tag !== "some" && v.tag !== "none" || !("val" in v)) && (ctx.vals[0] = {
			tag: "some",
			val: v
		}) : ctx.vals[0] = {
			tag: "some",
			val: v
		};
		f(ctx);
	};
}
function __jcoEmbeddedRuntime$lowerFlatResult(_context, meta) {
	let f = __jcoEmbeddedRuntime$lowerFlatVariant(_context, meta);
	return function lowerFlatResultInner(ctx) {
		__jcoEmbeddedRuntime$debugLog("[_lowerFlatResult()] args", { ctx });
		let v = ctx.vals[0];
		typeof v == "object" && v && Object.keys(v).length === 2 && "tag" in v && (v.tag === "ok" || v.tag === "err") && "val" in v || (ctx.vals[0] = {
			tag: "ok",
			val: v
		});
		f(ctx);
	};
}
function __jcoEmbeddedRuntime$initializeFutureValueClass(disposeSymbol) {
	var _a;
	class FutureValue {
		#start;
		#dropFn;
		#dropped = !1;
		#delivered = !1;
		#settled;
		#hideThen = 0;
		#thenFn;
		constructor(start, dropFn) {
			if (typeof start != "function") throw TypeError("future start operation must be a function");
			this.#start = start;
			this.#dropFn = dropFn;
			this.#thenFn = this.#then.bind(this);
		}
		get then() {
			return this.#hideThen === 0 ? this.#thenFn : void 0;
		}
		#read() {
			this.#settled ||= Promise.resolve().then(this.#start);
			return this.#settled;
		}
		resolveAsValue(resolve) {
			this.#hideThen++;
			try {
				resolve(this);
			} finally {
				this.#hideThen--;
			}
		}
		#deliver(resolve, value) {
			if (value instanceof _a) {
				value.resolveAsValue(resolve);
				return;
			}
			resolve(value);
		}
		#then(resolve, reject) {
			let droppedError = () => Error("future was dropped");
			return (this.#dropped ? Promise.reject(droppedError()) : this.#read()).then((box) => {
				if (this.#dropped) {
					if (typeof reject == "function") return reject(droppedError());
					throw droppedError();
				}
				this.#delivered = !0;
				return this.#deliver(resolve, box.value);
			}, reject);
		}
		[disposeSymbol]() {
			if (this.#dropped) return;
			this.#dropped = !0;
			this.#dropFn?.();
			this.#settled?.then((box) => {
				this.#delivered || __jcoEmbeddedRuntime$disposeHostValue(disposeSymbol, box.value);
			}, () => {});
		}
	}
	_a = FutureValue;
	return FutureValue;
}
const createNewCurrentImplicitThread = __jcoEmbeddedRuntime$createNewCurrentImplicitThread;
const getCurrentTask = __jcoEmbeddedRuntime$getCurrentTask;
const getCurrentThread = __jcoEmbeddedRuntime$getCurrentThread;
const _jcoInitializeAsyncSubtaskClass = __jcoEmbeddedRuntime$initializeAsyncSubtaskClass;
const _jcoInitializeAsyncTaskClass = __jcoEmbeddedRuntime$initializeAsyncTaskClass;
const _jcoInitializeAsyncThreadClass = __jcoEmbeddedRuntime$initializeAsyncThreadClass;
const _jcoInitializeCurrentComponentIdxs = __jcoEmbeddedRuntime$initializeCurrentComponentIdxs;
const _jcoInitializeCurrentTaskIds = __jcoEmbeddedRuntime$initializeCurrentTaskIds;
const _jcoInitializeCurrentTaskMayBlock = __jcoEmbeddedRuntime$initializeCurrentTaskMayBlock;
const _jcoInitializeTasksByComponent = __jcoEmbeddedRuntime$initializeTasksByComponent;
const _lowerImportBackwardsCompat = __jcoEmbeddedRuntime$lowerImportBackwardsCompat;
const _checkIdle = __jcoEmbeddedRuntime$checkIdle;
const getOrCreateAsyncState = __jcoEmbeddedRuntime$getOrCreateAsyncState;
const _guardTrap = __jcoEmbeddedRuntime$guardTrap;
const _jcoInitializeComponentAsyncStateClass = __jcoEmbeddedRuntime$initializeComponentAsyncStateClass;
const _jcoInitializeComponentAsyncStates = __jcoEmbeddedRuntime$initializeComponentAsyncStates;
const _jcoInitializeCoreTrapMessages = __jcoEmbeddedRuntime$initializeCoreTrapMessages;
const _jcoInitializeInstanceFlags = __jcoEmbeddedRuntime$initializeInstanceFlags;
const _jcoInitializeStoreAsyncState = __jcoEmbeddedRuntime$initializeStoreAsyncState;
const _jcoInitializeStoreTrap = __jcoEmbeddedRuntime$initializeStoreTrap;
const _postReturnTrap = __jcoEmbeddedRuntime$postReturnTrap;
const _reportTrap = __jcoEmbeddedRuntime$reportTrap;
const _stopInstance = __jcoEmbeddedRuntime$stopInstance;
const _trackHostOperation = __jcoEmbeddedRuntime$trackHostOperation;
const _whenIdle = __jcoEmbeddedRuntime$whenIdle;
const _requireValidNumericPrimitive = __jcoEmbeddedRuntime$requireValidNumericPrimitive;
const toInt8 = __jcoEmbeddedRuntime$toInt8;
const toUint16 = __jcoEmbeddedRuntime$toUint16;
const toUint32 = __jcoEmbeddedRuntime$toUint32;
const toUint64 = __jcoEmbeddedRuntime$toUint64;
const toUint8 = __jcoEmbeddedRuntime$toUint8;
const clampGuest = __jcoEmbeddedRuntime$clampGuest;
const _debugLog = __jcoEmbeddedRuntime$debugLog;
const _getGlobalCurrentThreadMeta = __jcoEmbeddedRuntime$getCurrentTaskMeta;
const getErrorPayload = __jcoEmbeddedRuntime$getErrorPayload;
const _jcoInitializeAsyncDeterminism = __jcoEmbeddedRuntime$initializeAsyncDeterminism;
const _jcoInitializeAsyncEventCodes = __jcoEmbeddedRuntime$initializeAsyncEventCodes;
const _jcoInitializeComponentErrorClass = __jcoEmbeddedRuntime$initializeComponentErrorClass;
const _jcoInitializeComponentMemoryMap = __jcoEmbeddedRuntime$initializeComponentMemoryMap;
const _jcoInitializeEndianness = __jcoEmbeddedRuntime$initializeEndianness;
const _jcoInitializeHasOwnProperty = __jcoEmbeddedRuntime$initializeHasOwnProperty;
const _jcoInitializeRepTableClass = __jcoEmbeddedRuntime$initializeRepTableClass;
const _jcoInitializeRuntimeErrorConstructor = __jcoEmbeddedRuntime$initializeRuntimeErrorConstructor;
const _jcoInitializeSymbolDispose = __jcoEmbeddedRuntime$initializeSymbolDispose;
const _jcoInitializeTaskRegisters = __jcoEmbeddedRuntime$initializeTaskRegisters;
const throwInvalidBool = __jcoEmbeddedRuntime$throwInvalidBool;
const _withGlobalCurrentThreadMeta = __jcoEmbeddedRuntime$withCurrentTaskMeta;
const _jcoInitializeFutureValueClass = __jcoEmbeddedRuntime$initializeFutureValueClass;
const _jcoInitializeFutures = __jcoEmbeddedRuntime$initializeFutures;
const dataView = __jcoEmbeddedRuntime$dataView;
const _jcoInitializeDataViewCache = __jcoEmbeddedRuntime$initializeDataViewCache;
const _decodeUtf8 = __jcoEmbeddedRuntime$decodeUtf8;
const _jcoInitializeLiftVariant = __jcoEmbeddedRuntime$initializeLiftVariant;
const _liftFlatBool = __jcoEmbeddedRuntime$liftFlatBool;
const _liftFlatList = __jcoEmbeddedRuntime$liftFlatList;
const _liftFlatOption = __jcoEmbeddedRuntime$liftFlatOption;
const _liftFlatRecord = __jcoEmbeddedRuntime$liftFlatRecord;
const _liftFlatStringAny = __jcoEmbeddedRuntime$liftFlatStringAny;
const _liftFlatU16 = __jcoEmbeddedRuntime$liftFlatU16;
const _liftFlatU32 = __jcoEmbeddedRuntime$liftFlatU32;
const _liftFlatU64 = __jcoEmbeddedRuntime$liftFlatU64;
const _liftFlatU8 = __jcoEmbeddedRuntime$liftFlatU8;
const _lowerFlatBool = __jcoEmbeddedRuntime$lowerFlatBool;
const _lowerFlatList = __jcoEmbeddedRuntime$lowerFlatList;
const _lowerFlatOption = __jcoEmbeddedRuntime$lowerFlatOption;
const _lowerFlatRecord = __jcoEmbeddedRuntime$lowerFlatRecord;
const _lowerFlatResult = __jcoEmbeddedRuntime$lowerFlatResult;
const _lowerFlatS8 = __jcoEmbeddedRuntime$lowerFlatS8;
const _lowerFlatStringAny = __jcoEmbeddedRuntime$lowerFlatStringAny;
const _lowerFlatU16 = __jcoEmbeddedRuntime$lowerFlatU16;
const _lowerFlatU32 = __jcoEmbeddedRuntime$lowerFlatU32;
const _lowerFlatU64 = __jcoEmbeddedRuntime$lowerFlatU64;
const _lowerFlatU8 = __jcoEmbeddedRuntime$lowerFlatU8;
const _jcoInitializeHandleIndexSpaces = __jcoEmbeddedRuntime$initializeHandleIndexSpaces;
const _jcoInitializeResourceScopeCounter = __jcoEmbeddedRuntime$initializeResourceScopeCounter;
const _jcoInitializeResourceScopeTasks = __jcoEmbeddedRuntime$initializeResourceScopeTasks;
const _jcoInitializeStreams = __jcoEmbeddedRuntime$initializeStreams;
const _jcoInitializeUtf16Decoder = __jcoEmbeddedRuntime$initializeUtf16Decoder;
const _jcoInitializeUtf16Encoding = __jcoEmbeddedRuntime$initializeUtf16Encoding;
const _jcoInitializeUtf8Decoder = __jcoEmbeddedRuntime$initializeUtf8Decoder;
const _jcoInitializeUtf8Encoder = __jcoEmbeddedRuntime$initializeUtf8Encoder;
const _utf8AllocateAndEncode = __jcoEmbeddedRuntime$utf8Encode;
const _jcoInitializeWaitableClass = __jcoEmbeddedRuntime$initializeWaitableClass;
export function instantiate(getCoreModule, imports, instantiateCore, options) {
	let WebAssembly = options?.shim?.globals?.WebAssembly ?? globalThis.WebAssembly;
	if (instantiateCore === void 0) {
		if (WebAssembly === void 0) throw Error("no WebAssembly implementation is available: pass instantiateCore or options.shim.globals.WebAssembly");
		instantiateCore = (module, importObject) => new WebAssembly.Instance(module, importObject);
	}
	let _jcoDataViewCacheContext = _jcoInitializeDataViewCache();
	let _jcoStrictNumericContext = { strict: !1 };
	let TEXT_DECODER_UTF8 = _jcoInitializeUtf8Decoder();
	let TEXT_ENCODER_UTF8 = _jcoInitializeUtf8Encoder();
	let ASYNC_TASKS_BY_COMPONENT_IDX = _jcoInitializeTasksByComponent();
	let CURRENT_TASK_META = _jcoInitializeTaskRegisters();
	let _jcoTaskLookupContext = {
		tasks: ASYNC_TASKS_BY_COMPONENT_IDX,
		registers: CURRENT_TASK_META
	};
	let ASYNC_STATE = _jcoInitializeComponentAsyncStates();
	let Waitable = _jcoInitializeWaitableClass();
	let _jcoResourceTableGetContext = { RuntimeError: WebAssembly.RuntimeError };
	let RepTable = _jcoInitializeRepTableClass(_jcoResourceTableGetContext);
	let HANDLE_INDEX_SPACES = _jcoInitializeHandleIndexSpaces();
	let INSTANCE_FLAGS = _jcoInitializeInstanceFlags();
	let STORE_TRAP = _jcoInitializeStoreTrap(options?.onTrap ?? null);
	let CORE_TRAP_MESSAGES = _jcoInitializeCoreTrapMessages({
		unreachable: "wasm trap: wasm `unreachable` instruction executed",
		"memory access out of bounds": "wasm trap: out of bounds memory access",
		"divide by zero": "wasm trap: integer divide by zero",
		"remainder by zero": "wasm trap: integer divide by zero",
		"divide result unrepresentable": "wasm trap: integer overflow",
		"float unrepresentable in integer range": "wasm trap: invalid conversion to integer",
		"table index is out of bounds": "wasm trap: undefined element: out of bounds table access",
		"function signature mismatch": "wasm trap: indirect call type mismatch",
		"call stack exhausted": "wasm trap: call stack exhausted"
	});
	let _jcoComponentNormalizeTrapContext = {
		RuntimeError: WebAssembly.RuntimeError,
		messages: CORE_TRAP_MESSAGES
	};
	let STORE_ASYNC_STATE = _jcoInitializeStoreAsyncState();
	let _jcoComponentDeadlockContext = {
		RuntimeError: WebAssembly.RuntimeError,
		storeTrap: STORE_TRAP,
		storeAsyncState: STORE_ASYNC_STATE,
		asyncState: ASYNC_STATE
	};
	let _jcoComponentStateLookupContext = {
		asyncState: ASYNC_STATE,
		ComponentAsyncState: _jcoInitializeComponentAsyncStateClass({
			RuntimeError: WebAssembly.RuntimeError,
			RepTable,
			indexSpaces: HANDLE_INDEX_SPACES,
			instanceFlags: INSTANCE_FLAGS,
			storeTrap: STORE_TRAP,
			normalization: _jcoComponentNormalizeTrapContext,
			cannotEnter: "wasm trap: cannot enter component instance",
			deadlock: _jcoComponentDeadlockContext,
			Waitable,
			reportTrap: (err) => _reportTrap(_jcoComponentReportTrapContext, err),
			checkIdle: () => _checkIdle(_jcoComponentIdleContext)
		})
	};
	let _jcoThreadLookupContext = {
		lookup: _jcoComponentStateLookupContext,
		registers: CURRENT_TASK_META
	};
	let GLOBAL_COMPONENT_MEMORY_MAP = _jcoInitializeComponentMemoryMap();
	let symbolDispose = _jcoInitializeSymbolDispose();
	let AsyncSubtask = _jcoInitializeAsyncSubtaskClass({
		RuntimeError: WebAssembly.RuntimeError,
		lookup: _jcoComponentStateLookupContext,
		memories: GLOBAL_COMPONENT_MEMORY_MAP,
		disposeSymbol: symbolDispose
	});
	let _jcoComponentPostReturnTrapContext = {
		RuntimeError: WebAssembly.RuntimeError,
		lookup: _jcoComponentStateLookupContext,
		stackOverflow: "wasm trap: call stack exhausted",
		uncaughtException: "wasm trap: uncaught exception propagated out of component"
	};
	let _jcoComponentRecordTrapContext = {
		RuntimeError: WebAssembly.RuntimeError,
		registers: CURRENT_TASK_META,
		lookup: _jcoComponentStateLookupContext
	};
	let WebAssemblyRuntimeError = _jcoInitializeRuntimeErrorConstructor(WebAssembly);
	let ASYNC_EVENT_CODE = _jcoInitializeAsyncEventCodes();
	let ASYNC_DETERMINISM = _jcoInitializeAsyncDeterminism("random");
	let _jcoResourceScopeCounterContext = _jcoInitializeResourceScopeCounter();
	let RESOURCE_SCOPE_TASKS = _jcoInitializeResourceScopeTasks();
	let ASYNC_CURRENT_TASK_IDS = _jcoInitializeCurrentTaskIds();
	let ASYNC_CURRENT_COMPONENT_IDXS = _jcoInitializeCurrentComponentIdxs();
	let _jcoTaskClearContext = {
		tasks: ASYNC_TASKS_BY_COMPONENT_IDX,
		taskIds: ASYNC_CURRENT_TASK_IDS,
		componentIdxs: ASYNC_CURRENT_COMPONENT_IDXS,
		checkIdle: () => _checkIdle(_jcoComponentIdleContext)
	};
	let FutureValue = _jcoInitializeFutureValueClass(symbolDispose);
	let _jcoTaskCreateContext = {
		AsyncTask: _jcoInitializeAsyncTaskClass({
			RuntimeError: WebAssemblyRuntimeError,
			postReturn: _jcoComponentPostReturnTrapContext,
			traps: _jcoComponentRecordTrapContext,
			AsyncSubtask,
			Waitable,
			lookup: _jcoComponentStateLookupContext,
			events: ASYNC_EVENT_CODE,
			determinism: ASYNC_DETERMINISM,
			scopeCounter: _jcoResourceScopeCounterContext,
			scopeTasks: RESOURCE_SCOPE_TASKS,
			storeTrap: STORE_TRAP,
			clearing: _jcoTaskClearContext,
			registers: CURRENT_TASK_META,
			FutureValue
		}),
		AsyncThread: _jcoInitializeAsyncThreadClass({
			lookup: _jcoComponentStateLookupContext,
			registers: CURRENT_TASK_META,
			traps: _jcoComponentRecordTrapContext
		}),
		tasks: ASYNC_TASKS_BY_COMPONENT_IDX,
		taskIds: ASYNC_CURRENT_TASK_IDS,
		componentIdxs: ASYNC_CURRENT_COMPONENT_IDXS
	};
	let _jcoTaskCompatImportContext = {
		tasks: ASYNC_TASKS_BY_COMPONENT_IDX,
		registers: CURRENT_TASK_META,
		lookup: _jcoComponentStateLookupContext,
		creation: _jcoTaskCreateContext,
		mayLeave: {
			RuntimeError: WebAssembly.RuntimeError,
			instanceFlags: INSTANCE_FLAGS
		},
		events: ASYNC_EVENT_CODE
	};
	let CURRENT_TASK_MAY_BLOCK = _jcoInitializeCurrentTaskMayBlock(WebAssembly !== void 0 && { Global: WebAssembly.Global });
	let _jcoLiftStringContext = {
		RuntimeError: WebAssemblyRuntimeError,
		utf8: TEXT_DECODER_UTF8,
		utf16: _jcoInitializeUtf16Decoder()
	};
	let isLE = _jcoInitializeEndianness();
	let _jcoLiftListContext = {
		RuntimeError: WebAssemblyRuntimeError,
		littleEndian: isLE
	};
	let _jcoLiftVariantContext = _jcoInitializeLiftVariant();
	let _jcoLowerStringContext = {
		utf8: TEXT_ENCODER_UTF8,
		utf16: _jcoInitializeUtf16Encoding()
	};
	let _jcoLowerListContext = {
		numeric: _jcoStrictNumericContext,
		littleEndian: isLE
	};
	let STREAMS = _jcoInitializeStreams(RepTable);
	let FUTURES = _jcoInitializeFutures(RepTable);
	let _jcoComponentStopContext = {
		RuntimeError: WebAssembly.RuntimeError,
		storeTrap: STORE_TRAP,
		storeAsyncState: STORE_ASYNC_STATE,
		asyncState: ASYNC_STATE,
		tasks: ASYNC_TASKS_BY_COMPONENT_IDX,
		symbolDispose,
		RepTable,
		channels: [STREAMS, FUTURES]
	};
	let _jcoComponentIdleContext = {
		storeTrap: STORE_TRAP,
		storeAsyncState: STORE_ASYNC_STATE,
		tasks: ASYNC_TASKS_BY_COMPONENT_IDX
	};
	let _jcoComponentReportTrapContext = {
		RuntimeError: WebAssembly.RuntimeError,
		storeTrap: STORE_TRAP,
		normalization: _jcoComponentNormalizeTrapContext
	};
	let _jcoComponentHostOperationContext = {
		RuntimeError: WebAssembly.RuntimeError,
		storeTrap: STORE_TRAP,
		storeAsyncState: STORE_ASYNC_STATE,
		asyncState: ASYNC_STATE,
		symbolDispose
	};
	let ComponentError = _jcoInitializeComponentErrorClass();
	let hasOwnProperty = _jcoInitializeHasOwnProperty();
	WebAssembly.RuntimeError;
	let module0 = getCoreModule("board-v2.core.wasm");
	let module1 = getCoreModule("board-v2.core2.wasm");
	let module2 = getCoreModule("board-v2.core3.wasm");
	let { analogRead, bleAdvertise, bleConnect, bleDisconnect, bleDiscover, bleNotifications, bleNotify, bleRead, bleScan, bleSetGatt, bleStopAdvertising, bleSubscribe, bleTakeWrites, bleWrite, console, i2cTransfer, radioConfigure, radioReceive, radioSend, tcpClose, tcpConnect, tcpRead, tcpWrite, uartWrite, wifiJoin, wifiScan } = imports["litz:mcu/host"];
	if (analogRead === void 0) {
		let err = Error("unexpectedly undefined instance import 'analogRead', was 'analogRead' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleAdvertise === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleAdvertise', was 'bleAdvertise' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleConnect === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleConnect', was 'bleConnect' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleDisconnect === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleDisconnect', was 'bleDisconnect' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleDiscover === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleDiscover', was 'bleDiscover' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleNotifications === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleNotifications', was 'bleNotifications' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleNotify === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleNotify', was 'bleNotify' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleRead === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleRead', was 'bleRead' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleScan === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleScan', was 'bleScan' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleSetGatt === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleSetGatt', was 'bleSetGatt' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleStopAdvertising === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleStopAdvertising', was 'bleStopAdvertising' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleSubscribe === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleSubscribe', was 'bleSubscribe' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleTakeWrites === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleTakeWrites', was 'bleTakeWrites' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (bleWrite === void 0) {
		let err = Error("unexpectedly undefined instance import 'bleWrite', was 'bleWrite' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (console === void 0) {
		let err = Error("unexpectedly undefined instance import 'console', was 'console' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (i2cTransfer === void 0) {
		let err = Error("unexpectedly undefined instance import 'i2cTransfer', was 'i2cTransfer' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (radioConfigure === void 0) {
		let err = Error("unexpectedly undefined instance import 'radioConfigure', was 'radioConfigure' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (radioReceive === void 0) {
		let err = Error("unexpectedly undefined instance import 'radioReceive', was 'radioReceive' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (radioSend === void 0) {
		let err = Error("unexpectedly undefined instance import 'radioSend', was 'radioSend' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (tcpClose === void 0) {
		let err = Error("unexpectedly undefined instance import 'tcpClose', was 'tcpClose' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (tcpConnect === void 0) {
		let err = Error("unexpectedly undefined instance import 'tcpConnect', was 'tcpConnect' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (tcpRead === void 0) {
		let err = Error("unexpectedly undefined instance import 'tcpRead', was 'tcpRead' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (tcpWrite === void 0) {
		let err = Error("unexpectedly undefined instance import 'tcpWrite', was 'tcpWrite' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (uartWrite === void 0) {
		let err = Error("unexpectedly undefined instance import 'uartWrite', was 'uartWrite' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (wifiJoin === void 0) {
		let err = Error("unexpectedly undefined instance import 'wifiJoin', was 'wifiJoin' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	if (wifiScan === void 0) {
		let err = Error("unexpectedly undefined instance import 'wifiScan', was 'wifiScan' available at instantiation?");
		console.error("ERROR:", err.toString());
		throw err;
	}
	let gen = (function* _initGenerator() {
		let instanceFlags0 = new WebAssembly.Global({
			value: "i32",
			mutable: !0
		}, 1);
		INSTANCE_FLAGS.set(0, instanceFlags0);
		let exports0;
		let _trampoline0 = function(arg0) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-disconnect\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleDisconnect",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleDisconnect(clampGuest(arg0, 0, 65535)))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-disconnect\"][Instruction::Return]", {
				funcName: "ble-disconnect",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline0.fnName = "litz:mcu/host@0.2.0#bleDisconnect";
		let _trampoline1 = function(arg0) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-close\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "tcpClose",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => tcpClose(arg0 >>> 0))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-close\"][Instruction::Return]", {
				funcName: "tcp-close",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline1.fnName = "litz:mcu/host@0.2.0#tcpClose";
		let exports1;
		let memory0;
		let realloc0;
		let _trampoline2 = function(arg0, arg1) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"console\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "console",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => console(result0))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"console\"][Instruction::Return]", {
				funcName: "console",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline2.fnName = "litz:mcu/host@0.2.0#console";
		let _trampoline3 = function(arg0, arg1, arg2, arg3, arg4, arg5) {
			var ptr0 = arg2 >>> 0 >>> 0;
			var len0 = arg3;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"i2c-transfer\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "i2cTransfer",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => i2cTransfer(clampGuest(arg0, 0, 255), clampGuest(arg1, 0, 255), result0, arg4 >>> 0))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant4 = ret;
			switch (variant4.tag) {
				case "ok": {
					let e = variant4.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 0, !0);
					var val2 = e;
					var len2 = Array.isArray(val2) ? val2.length : val2.byteLength;
					var ptr2 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len2 * 1);
					ptr2 >>>= 0;
					if (ptr2 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - ptr2) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData2;
					let valLenBytes2 = len2 * 1;
					if (Array.isArray(val2)) {
						let offset = 0;
						let dv2 = new DataView(memory0.buffer);
						for (let v of val2) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv2.setUint8(ptr2 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData2 = new Uint8Array(val2.buffer || val2, val2.byteOffset, valLenBytes2);
						new Uint8Array(memory0.buffer, ptr2, valLenBytes2).set(valData2);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 4, ptr2, !0);
					break;
				}
				case "err": {
					let e = variant4.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr3 = encodeRes.ptr;
					var len3 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 8, len3, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 4, ptr3, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant4,
						valueType: typeof variant4
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"i2c-transfer\"][Instruction::Return]", {
				funcName: "i2c-transfer",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline3.fnName = "litz:mcu/host@0.2.0#i2cTransfer";
		let _trampoline4 = function(arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			var ptr1 = arg2 >>> 0 >>> 0;
			var len1 = arg3;
			if (ptr1 < 0 || len1 < 0 || len1 > 268435455 || ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr1 % 1 != 0) throw TypeError(`list pointer [${ptr1}] is not aligned to 1`);
			var result1 = new Uint8Array(memory0.buffer.slice(ptr1, ptr1 + len1 * 1));
			var ptr2 = arg4 >>> 0 >>> 0;
			var len2 = arg5;
			if (ptr2 < 0 || len2 < 0 || len2 > 268435455 || ptr2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - ptr2) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr2 % 1 != 0) throw TypeError(`list pointer [${ptr2}] is not aligned to 1`);
			var result2 = new Uint8Array(memory0.buffer.slice(ptr2, ptr2 + len2 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-advertise\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleAdvertise",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet3 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleAdvertise(result0, result1, result2))
				});
				ret = typeof hostRet3 == "object" && hostRet3 && (hostRet3.tag === "ok" || hostRet3.tag === "err") ? hostRet3 : {
					tag: "ok",
					val: hostRet3
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant6 = ret;
			switch (variant6.tag) {
				case "ok": {
					let e = variant6.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg6 >>> 0) + 0, 0, !0);
					var val4 = e;
					var len4 = Array.isArray(val4) ? val4.length : val4.byteLength;
					var ptr4 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len4 * 1);
					ptr4 >>>= 0;
					if (ptr4 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr4 > memory0.buffer.byteLength || len4 > Math.floor((memory0.buffer.byteLength - ptr4) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData4;
					let valLenBytes4 = len4 * 1;
					if (Array.isArray(val4)) {
						let offset = 0;
						let dv4 = new DataView(memory0.buffer);
						for (let v of val4) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv4.setUint8(ptr4 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData4 = new Uint8Array(val4.buffer || val4, val4.byteOffset, valLenBytes4);
						new Uint8Array(memory0.buffer, ptr4, valLenBytes4).set(valData4);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg6 >>> 0) + 8, len4, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg6 >>> 0) + 4, ptr4, !0);
					break;
				}
				case "err": {
					let e = variant6.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg6 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr5 = encodeRes.ptr;
					var len5 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg6 >>> 0) + 8, len5, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg6 >>> 0) + 4, ptr5, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant6,
						valueType: typeof variant6
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-advertise\"][Instruction::Return]", {
				funcName: "ble-advertise",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline4.fnName = "litz:mcu/host@0.2.0#bleAdvertise";
		let _trampoline5 = function(arg0, arg1, arg2, arg3, arg4, arg5) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1 >>> 0;
			if (ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
			var result0 = _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr0, len0));
			var ptr1 = arg3 >>> 0 >>> 0;
			var len1 = arg4;
			if (ptr1 < 0 || len1 < 0 || len1 > 268435455 || ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr1 % 1 != 0) throw TypeError(`list pointer [${ptr1}] is not aligned to 1`);
			var result1 = new Uint8Array(memory0.buffer.slice(ptr1, ptr1 + len1 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"radio-configure\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "radioConfigure",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet2 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => radioConfigure(result0, clampGuest(arg2, 0, 65535), result1))
				});
				ret = typeof hostRet2 == "object" && hostRet2 && (hostRet2.tag === "ok" || hostRet2.tag === "err") ? hostRet2 : {
					tag: "ok",
					val: hostRet2
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant4 = ret;
			switch (variant4.tag) {
				case "ok":
					variant4.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 0, !0);
					break;
				case "err": {
					let e = variant4.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr3 = encodeRes.ptr;
					var len3 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 8, len3, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 4, ptr3, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant4,
						valueType: typeof variant4
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"radio-configure\"][Instruction::Return]", {
				funcName: "radio-configure",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline5.fnName = "litz:mcu/host@0.2.0#radioConfigure";
		let _trampoline6 = function(arg0, arg1, arg2) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-connect\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleConnect",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleConnect(result0))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg2 >>> 0) + 0, 0, !0);
					dataView(_jcoDataViewCacheContext, memory0).setInt16((arg2 >>> 0) + 4, toUint16(_jcoStrictNumericContext, e), !0);
					break;
				}
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg2 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-connect\"][Instruction::Return]", {
				funcName: "ble-connect",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline6.fnName = "litz:mcu/host@0.2.0#bleConnect";
		let _trampoline7 = function(arg0, arg1) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-stop-advertising\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleStopAdvertising",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleStopAdvertising(result0))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-stop-advertising\"][Instruction::Return]", {
				funcName: "ble-stop-advertising",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline7.fnName = "litz:mcu/host@0.2.0#bleStopAdvertising";
		let _trampoline8 = function(arg0, arg1, arg2) {
			var ptr0 = arg1 >>> 0 >>> 0;
			var len0 = arg2;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"uart-write\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "uartWrite",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => uartWrite(clampGuest(arg0, 0, 255), result0))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"uart-write\"][Instruction::Return]", {
				funcName: "uart-write",
				paramCount: 1,
				async: !1,
				postReturn: !1
			});
			task.resolve([+!!ret]);
			task.exit();
			return +!!ret;
		};
		_trampoline8.fnName = "litz:mcu/host@0.2.0#uartWrite";
		let _trampoline9 = function(arg0, arg1, arg2, arg3, arg4) {
			var ptr0 = arg2 >>> 0 >>> 0;
			var len0 = arg3;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-notify\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleNotify",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleNotify(clampGuest(arg0, 0, 65535), clampGuest(arg1, 0, 65535), result0))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok":
					variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg4 >>> 0) + 0, 0, !0);
					break;
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg4 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg4 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg4 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-notify\"][Instruction::Return]", {
				funcName: "ble-notify",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline9.fnName = "litz:mcu/host@0.2.0#bleNotify";
		let _trampoline10 = function(arg0, arg1, arg2, arg3, arg4, arg5) {
			var ptr0 = arg3 >>> 0 >>> 0;
			var len0 = arg4;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-write\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleWrite",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleWrite(clampGuest(arg0, 0, 65535), clampGuest(arg1, 0, 65535), clampGuest(arg2, 0, 65535), result0))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok":
					variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 0, !0);
					break;
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-write\"][Instruction::Return]", {
				funcName: "ble-write",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline10.fnName = "litz:mcu/host@0.2.0#bleWrite";
		let _trampoline11 = function(arg0, arg1, arg2, arg3) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-connect\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "tcpConnect",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => tcpConnect(result0, clampGuest(arg2, 0, 65535)))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg3 >>> 0) + 0, 0, !0);
					dataView(_jcoDataViewCacheContext, memory0).setInt32((arg3 >>> 0) + 4, toUint32(_jcoStrictNumericContext, e), !0);
					break;
				}
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg3 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-connect\"][Instruction::Return]", {
				funcName: "tcp-connect",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline11.fnName = "litz:mcu/host@0.2.0#tcpConnect";
		let _trampoline12 = function(arg0, arg1, arg2, arg3) {
			var ptr0 = arg1 >>> 0 >>> 0;
			var len0 = arg2;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-write\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "tcpWrite",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => tcpWrite(arg0 >>> 0, result0))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg3 >>> 0) + 0, 0, !0);
					dataView(_jcoDataViewCacheContext, memory0).setInt32((arg3 >>> 0) + 4, toUint32(_jcoStrictNumericContext, e), !0);
					break;
				}
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg3 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-write\"][Instruction::Return]", {
				funcName: "tcp-write",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline12.fnName = "litz:mcu/host@0.2.0#tcpWrite";
		let _trampoline13 = function(arg0, arg1, arg2) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-read\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "tcpRead",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet0 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => tcpRead(arg0 >>> 0, arg1 >>> 0))
				});
				ret = typeof hostRet0 == "object" && hostRet0 && (hostRet0.tag === "ok" || hostRet0.tag === "err") ? hostRet0 : {
					tag: "ok",
					val: hostRet0
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg2 >>> 0) + 0, 0, !0);
					var val1 = e;
					var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
					var ptr1 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len1 * 1);
					ptr1 >>>= 0;
					if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData1;
					let valLenBytes1 = len1 * 1;
					if (Array.isArray(val1)) {
						let offset = 0;
						let dv1 = new DataView(memory0.buffer);
						for (let v of val1) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv1.setUint8(ptr1 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
						new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 8, len1, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 4, ptr1, !0);
					break;
				}
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg2 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"tcp-read\"][Instruction::Return]", {
				funcName: "tcp-read",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline13.fnName = "litz:mcu/host@0.2.0#tcpRead";
		let _trampoline14 = function(arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1;
			if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
			var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
			let variant2;
			switch (arg2) {
				case 0:
					variant2 = void 0;
					break;
				case 1:
					var ptr1 = arg3 >>> 0 >>> 0;
					var len1 = arg4;
					if (ptr1 < 0 || len1 < 0 || len1 > 268435455 || ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
					if (ptr1 % 1 != 0) throw TypeError(`list pointer [${ptr1}] is not aligned to 1`);
					variant2 = new Uint8Array(memory0.buffer.slice(ptr1, ptr1 + len1 * 1));
					break;
				default: throw TypeError("invalid variant discriminant for option");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"radio-send\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "radioSend",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet3 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => radioSend(result0, variant2, BigInt.asUintN(64, BigInt(arg5))))
				});
				ret = typeof hostRet3 == "object" && hostRet3 && (hostRet3.tag === "ok" || hostRet3.tag === "err") ? hostRet3 : {
					tag: "ok",
					val: hostRet3
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant5 = ret;
			switch (variant5.tag) {
				case "ok":
					variant5.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg6 >>> 0) + 0, 0, !0);
					break;
				case "err": {
					let e = variant5.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg6 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr4 = encodeRes.ptr;
					var len4 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg6 >>> 0) + 8, len4, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg6 >>> 0) + 4, ptr4, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant5,
						valueType: typeof variant5
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"radio-send\"][Instruction::Return]", {
				funcName: "radio-send",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline14.fnName = "litz:mcu/host@0.2.0#radioSend";
		let _trampoline15 = function(arg0, arg1) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"analog-read\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "analogRead",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => analogRead(clampGuest(arg0, 0, 255)))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant0 = ret;
			if (variant0 == null) dataView(_jcoDataViewCacheContext, memory0).setInt8((arg1 >>> 0) + 0, 0, !0);
			else {
				let e = variant0;
				dataView(_jcoDataViewCacheContext, memory0).setInt8((arg1 >>> 0) + 0, 1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt32((arg1 >>> 0) + 4, toUint32(_jcoStrictNumericContext, e), !0);
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"analog-read\"][Instruction::Return]", {
				funcName: "analog-read",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline15.fnName = "litz:mcu/host@0.2.0#analogRead";
		let _trampoline16 = function(arg0, arg1) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-discover\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleDiscover",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet0 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleDiscover(clampGuest(arg0, 0, 65535)))
				});
				ret = typeof hostRet0 == "object" && hostRet0 && (hostRet0.tag === "ok" || hostRet0.tag === "err") ? hostRet0 : {
					tag: "ok",
					val: hostRet0
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant7 = ret;
			switch (variant7.tag) {
				case "ok": {
					let e = variant7.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg1 >>> 0) + 0, 0, !0);
					var vec5 = e;
					var len5 = vec5.length;
					var result5 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 4, len5 * 12);
					result5 >>>= 0;
					if (result5 % 4 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (result5 > memory0.buffer.byteLength || len5 > Math.floor((memory0.buffer.byteLength - result5) / 12)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					if (result5 < 0 || len5 < 0 || len5 > 22369621 || result5 > memory0.buffer.byteLength || len5 > Math.floor((memory0.buffer.byteLength - result5) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
					for (let i = 0; i < vec5.length; i++) {
						let e = vec5[i];
						let base = result5 + i * 12;
						var { uuid: v1_0, characteristics: v1_1 } = e;
						dataView(_jcoDataViewCacheContext, memory0).setInt16(base + 0, toUint16(_jcoStrictNumericContext, v1_0), !0);
						var vec4 = v1_1;
						var len4 = vec4.length;
						var result4 = ((...args) => {
							let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
							let previousMayLeave = state.mayLeave;
							let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
							let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
							let previousStorage = thread?.storage;
							thread && (thread.storage = [0, 0]);
							state.mayLeave = !1;
							let restore = () => {
								state.mayLeave = previousMayLeave;
								thread && (thread.storage = previousStorage);
							};
							let result;
							try {
								result = realloc0(...args);
							} catch (err) {
								restore();
								throw err;
							}
							if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
							restore();
							return result;
						})(0, 0, 4, len4 * 12);
						result4 >>>= 0;
						if (result4 % 4 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
						if (result4 > memory0.buffer.byteLength || len4 > Math.floor((memory0.buffer.byteLength - result4) / 12)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
						if (result4 < 0 || len4 < 0 || len4 > 22369621 || result4 > memory0.buffer.byteLength || len4 > Math.floor((memory0.buffer.byteLength - result4) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
						for (let i = 0; i < vec4.length; i++) {
							let e = vec4[i];
							let base = result4 + i * 12;
							var { uuid: v2_0, access: v2_1, value: v2_2 } = e;
							dataView(_jcoDataViewCacheContext, memory0).setInt16(base + 0, toUint16(_jcoStrictNumericContext, v2_0), !0);
							dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 2, toUint8(_jcoStrictNumericContext, v2_1), !0);
							var val3 = v2_2;
							var len3 = Array.isArray(val3) ? val3.length : val3.byteLength;
							var ptr3 = ((...args) => {
								let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
								let previousMayLeave = state.mayLeave;
								let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
								let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
								let previousStorage = thread?.storage;
								thread && (thread.storage = [0, 0]);
								state.mayLeave = !1;
								let restore = () => {
									state.mayLeave = previousMayLeave;
									thread && (thread.storage = previousStorage);
								};
								let result;
								try {
									result = realloc0(...args);
								} catch (err) {
									restore();
									throw err;
								}
								if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
								restore();
								return result;
							})(0, 0, 1, len3 * 1);
							ptr3 >>>= 0;
							if (ptr3 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
							if (ptr3 > memory0.buffer.byteLength || len3 > Math.floor((memory0.buffer.byteLength - ptr3) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
							let valData3;
							let valLenBytes3 = len3 * 1;
							if (Array.isArray(val3)) {
								let offset = 0;
								let dv3 = new DataView(memory0.buffer);
								for (let v of val3) {
									_requireValidNumericPrimitive.bind(null, "u8")(v);
									dv3.setUint8(ptr3 + offset, v, !0);
									offset += 1;
								}
							} else {
								valData3 = new Uint8Array(val3.buffer || val3, val3.byteOffset, valLenBytes3);
								new Uint8Array(memory0.buffer, ptr3, valLenBytes3).set(valData3);
							}
							dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 8, len3, !0);
							dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, ptr3, !0);
						}
						dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 8, len4, !0);
						dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, result4, !0);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg1 >>> 0) + 8, len5, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg1 >>> 0) + 4, result5, !0);
					break;
				}
				case "err": {
					let e = variant7.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg1 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr6 = encodeRes.ptr;
					var len6 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg1 >>> 0) + 8, len6, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg1 >>> 0) + 4, ptr6, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant7,
						valueType: typeof variant7
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-discover\"][Instruction::Return]", {
				funcName: "ble-discover",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline16.fnName = "litz:mcu/host@0.2.0#bleDiscover";
		let _trampoline17 = function(arg0, arg1, arg2) {
			var len2 = arg1;
			var base2 = arg0 >>> 0 >>> 0;
			if (base2 < 0 || len2 < 0 || len2 > 22369621 || base2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - base2) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			if (base2 % 4 != 0) throw TypeError(`list pointer [${base2}] is not aligned to 4`);
			var result2 = [];
			for (let i = 0; i < len2; i++) {
				let base = base2 + i * 12;
				var len1 = dataView(_jcoDataViewCacheContext, memory0).getUint32(base + 8, !0);
				var base1 = dataView(_jcoDataViewCacheContext, memory0).getUint32(base + 4, !0) >>> 0;
				if (base1 < 0 || len1 < 0 || len1 > 22369621 || base1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - base1) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
				if (base1 % 4 != 0) throw TypeError(`list pointer [${base1}] is not aligned to 4`);
				var result1 = [];
				for (let i = 0; i < len1; i++) {
					let base = base1 + i * 12;
					var ptr0 = dataView(_jcoDataViewCacheContext, memory0).getUint32(base + 4, !0) >>> 0;
					var len0 = dataView(_jcoDataViewCacheContext, memory0).getUint32(base + 8, !0);
					if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
					if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
					var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
					result1.push({
						uuid: clampGuest(dataView(_jcoDataViewCacheContext, memory0).getUint16(base + 0, !0), 0, 65535),
						access: clampGuest(dataView(_jcoDataViewCacheContext, memory0).getUint8(base + 2, !0), 0, 255),
						value: result0
					});
				}
				result2.push({
					uuid: clampGuest(dataView(_jcoDataViewCacheContext, memory0).getUint16(base + 0, !0), 0, 65535),
					characteristics: result1
				});
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-set-gatt\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleSetGatt",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet3 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleSetGatt(result2))
				});
				ret = typeof hostRet3 == "object" && hostRet3 && (hostRet3.tag === "ok" || hostRet3.tag === "err") ? hostRet3 : {
					tag: "ok",
					val: hostRet3
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant5 = ret;
			switch (variant5.tag) {
				case "ok":
					variant5.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg2 >>> 0) + 0, 0, !0);
					break;
				case "err": {
					let e = variant5.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg2 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr4 = encodeRes.ptr;
					var len4 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 8, len4, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg2 >>> 0) + 4, ptr4, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant5,
						valueType: typeof variant5
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-set-gatt\"][Instruction::Return]", {
				funcName: "ble-set-gatt",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline17.fnName = "litz:mcu/host@0.2.0#bleSetGatt";
		let _trampoline18 = function(arg0, arg1, arg2, arg3, arg4) {
			var bool0 = arg3;
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-subscribe\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleSubscribe",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet1 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleSubscribe(clampGuest(arg0, 0, 65535), clampGuest(arg1, 0, 65535), clampGuest(arg2, 0, 65535), bool0 == 0 ? !1 : bool0 == 1 || throwInvalidBool()))
				});
				ret = typeof hostRet1 == "object" && hostRet1 && (hostRet1.tag === "ok" || hostRet1.tag === "err") ? hostRet1 : {
					tag: "ok",
					val: hostRet1
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok":
					variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg4 >>> 0) + 0, 0, !0);
					break;
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg4 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg4 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg4 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-subscribe\"][Instruction::Return]", {
				funcName: "ble-subscribe",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline18.fnName = "litz:mcu/host@0.2.0#bleSubscribe";
		let _trampoline19 = function(arg0) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"radio-receive\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "radioReceive",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => radioReceive())
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var vec5 = ret;
			var len5 = vec5.length;
			var result5 = ((...args) => {
				let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
				let previousMayLeave = state.mayLeave;
				let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
				let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
				let previousStorage = thread?.storage;
				thread && (thread.storage = [0, 0]);
				state.mayLeave = !1;
				let restore = () => {
					state.mayLeave = previousMayLeave;
					thread && (thread.storage = previousStorage);
				};
				let result;
				try {
					result = realloc0(...args);
				} catch (err) {
					restore();
					throw err;
				}
				if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
				restore();
				return result;
			})(0, 0, 8, len5 * 48);
			result5 >>>= 0;
			if (result5 % 8 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
			if (result5 > memory0.buffer.byteLength || len5 > Math.floor((memory0.buffer.byteLength - result5) / 48)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
			if (result5 < 0 || len5 < 0 || len5 > 5592405 || result5 > memory0.buffer.byteLength || len5 > Math.floor((memory0.buffer.byteLength - result5) / 48)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			for (let i = 0; i < vec5.length; i++) {
				let e = vec5[i];
				let base = result5 + i * 48;
				var { sender: v0_0, to: v0_1, bytes: v0_2, sentAt: v0_3, airtime: v0_4 } = e;
				var val1 = v0_0;
				var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
				var ptr1 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len1 * 1);
				ptr1 >>>= 0;
				if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData1;
				let valLenBytes1 = len1 * 1;
				if (Array.isArray(val1)) {
					let offset = 0;
					let dv1 = new DataView(memory0.buffer);
					for (let v of val1) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv1.setUint8(ptr1 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
					new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, len1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 0, ptr1, !0);
				var variant3 = v0_1;
				if (variant3 == null) dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 8, 0, !0);
				else {
					let e = variant3;
					dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 8, 1, !0);
					var val2 = e;
					var len2 = Array.isArray(val2) ? val2.length : val2.byteLength;
					var ptr2 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len2 * 1);
					ptr2 >>>= 0;
					if (ptr2 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - ptr2) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData2;
					let valLenBytes2 = len2 * 1;
					if (Array.isArray(val2)) {
						let offset = 0;
						let dv2 = new DataView(memory0.buffer);
						for (let v of val2) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv2.setUint8(ptr2 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData2 = new Uint8Array(val2.buffer || val2, val2.byteOffset, valLenBytes2);
						new Uint8Array(memory0.buffer, ptr2, valLenBytes2).set(valData2);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 16, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 12, ptr2, !0);
				}
				var val4 = v0_2;
				var len4 = Array.isArray(val4) ? val4.length : val4.byteLength;
				var ptr4 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len4 * 1);
				ptr4 >>>= 0;
				if (ptr4 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr4 > memory0.buffer.byteLength || len4 > Math.floor((memory0.buffer.byteLength - ptr4) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData4;
				let valLenBytes4 = len4 * 1;
				if (Array.isArray(val4)) {
					let offset = 0;
					let dv4 = new DataView(memory0.buffer);
					for (let v of val4) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv4.setUint8(ptr4 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData4 = new Uint8Array(val4.buffer || val4, val4.byteOffset, valLenBytes4);
					new Uint8Array(memory0.buffer, ptr4, valLenBytes4).set(valData4);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 24, len4, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 20, ptr4, !0);
				dataView(_jcoDataViewCacheContext, memory0).setBigInt64(base + 32, toUint64(_jcoStrictNumericContext, v0_3), !0);
				dataView(_jcoDataViewCacheContext, memory0).setBigInt64(base + 40, toUint64(_jcoStrictNumericContext, v0_4), !0);
			}
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 4, len5, !0);
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 0, result5, !0);
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"radio-receive\"][Instruction::Return]", {
				funcName: "radio-receive",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline19.fnName = "litz:mcu/host@0.2.0#radioReceive";
		let _trampoline20 = function(arg0) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-take-writes\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleTakeWrites",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleTakeWrites())
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var vec2 = ret;
			var len2 = vec2.length;
			var result2 = ((...args) => {
				let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
				let previousMayLeave = state.mayLeave;
				let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
				let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
				let previousStorage = thread?.storage;
				thread && (thread.storage = [0, 0]);
				state.mayLeave = !1;
				let restore = () => {
					state.mayLeave = previousMayLeave;
					thread && (thread.storage = previousStorage);
				};
				let result;
				try {
					result = realloc0(...args);
				} catch (err) {
					restore();
					throw err;
				}
				if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
				restore();
				return result;
			})(0, 0, 4, len2 * 12);
			result2 >>>= 0;
			if (result2 % 4 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
			if (result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 12)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
			if (result2 < 0 || len2 < 0 || len2 > 22369621 || result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			for (let i = 0; i < vec2.length; i++) {
				let e = vec2[i];
				let base = result2 + i * 12;
				var { service: v0_0, characteristic: v0_1, value: v0_2 } = e;
				dataView(_jcoDataViewCacheContext, memory0).setInt16(base + 0, toUint16(_jcoStrictNumericContext, v0_0), !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt16(base + 2, toUint16(_jcoStrictNumericContext, v0_1), !0);
				var val1 = v0_2;
				var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
				var ptr1 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len1 * 1);
				ptr1 >>>= 0;
				if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData1;
				let valLenBytes1 = len1 * 1;
				if (Array.isArray(val1)) {
					let offset = 0;
					let dv1 = new DataView(memory0.buffer);
					for (let v of val1) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv1.setUint8(ptr1 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
					new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 8, len1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, ptr1, !0);
			}
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 4, len2, !0);
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 0, result2, !0);
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-take-writes\"][Instruction::Return]", {
				funcName: "ble-take-writes",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline20.fnName = "litz:mcu/host@0.2.0#bleTakeWrites";
		let _trampoline21 = function(arg0, arg1) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-notifications\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleNotifications",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleNotifications(clampGuest(arg0, 0, 65535)))
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var vec2 = ret;
			var len2 = vec2.length;
			var result2 = ((...args) => {
				let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
				let previousMayLeave = state.mayLeave;
				let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
				let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
				let previousStorage = thread?.storage;
				thread && (thread.storage = [0, 0]);
				state.mayLeave = !1;
				let restore = () => {
					state.mayLeave = previousMayLeave;
					thread && (thread.storage = previousStorage);
				};
				let result;
				try {
					result = realloc0(...args);
				} catch (err) {
					restore();
					throw err;
				}
				if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
				restore();
				return result;
			})(0, 0, 4, len2 * 12);
			result2 >>>= 0;
			if (result2 % 4 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
			if (result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 12)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
			if (result2 < 0 || len2 < 0 || len2 > 22369621 || result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			for (let i = 0; i < vec2.length; i++) {
				let e = vec2[i];
				let base = result2 + i * 12;
				var { service: v0_0, characteristic: v0_1, value: v0_2 } = e;
				dataView(_jcoDataViewCacheContext, memory0).setInt16(base + 0, toUint16(_jcoStrictNumericContext, v0_0), !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt16(base + 2, toUint16(_jcoStrictNumericContext, v0_1), !0);
				var val1 = v0_2;
				var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
				var ptr1 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len1 * 1);
				ptr1 >>>= 0;
				if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData1;
				let valLenBytes1 = len1 * 1;
				if (Array.isArray(val1)) {
					let offset = 0;
					let dv1 = new DataView(memory0.buffer);
					for (let v of val1) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv1.setUint8(ptr1 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
					new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 8, len1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, ptr1, !0);
			}
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg1 >>> 0) + 4, len2, !0);
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg1 >>> 0) + 0, result2, !0);
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-notifications\"][Instruction::Return]", {
				funcName: "ble-notifications",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline21.fnName = "litz:mcu/host@0.2.0#bleNotifications";
		let _trampoline22 = function(arg0, arg1, arg2, arg3) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-read\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleRead",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet0 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleRead(clampGuest(arg0, 0, 65535), clampGuest(arg1, 0, 65535), clampGuest(arg2, 0, 65535)))
				});
				ret = typeof hostRet0 == "object" && hostRet0 && (hostRet0.tag === "ok" || hostRet0.tag === "err") ? hostRet0 : {
					tag: "ok",
					val: hostRet0
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant3 = ret;
			switch (variant3.tag) {
				case "ok": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg3 >>> 0) + 0, 0, !0);
					var val1 = e;
					var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
					var ptr1 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len1 * 1);
					ptr1 >>>= 0;
					if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData1;
					let valLenBytes1 = len1 * 1;
					if (Array.isArray(val1)) {
						let offset = 0;
						let dv1 = new DataView(memory0.buffer);
						for (let v of val1) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv1.setUint8(ptr1 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
						new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 8, len1, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 4, ptr1, !0);
					break;
				}
				case "err": {
					let e = variant3.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg3 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 8, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg3 >>> 0) + 4, ptr2, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant3,
						valueType: typeof variant3
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-read\"][Instruction::Return]", {
				funcName: "ble-read",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline22.fnName = "litz:mcu/host@0.2.0#bleRead";
		let _trampoline23 = function(arg0) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-scan\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "bleScan",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => bleScan())
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var vec6 = ret;
			var len6 = vec6.length;
			var result6 = ((...args) => {
				let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
				let previousMayLeave = state.mayLeave;
				let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
				let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
				let previousStorage = thread?.storage;
				thread && (thread.storage = [0, 0]);
				state.mayLeave = !1;
				let restore = () => {
					state.mayLeave = previousMayLeave;
					thread && (thread.storage = previousStorage);
				};
				let result;
				try {
					result = realloc0(...args);
				} catch (err) {
					restore();
					throw err;
				}
				if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
				restore();
				return result;
			})(0, 0, 4, len6 * 40);
			result6 >>>= 0;
			if (result6 % 4 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
			if (result6 > memory0.buffer.byteLength || len6 > Math.floor((memory0.buffer.byteLength - result6) / 40)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
			if (result6 < 0 || len6 < 0 || len6 > 6710886 || result6 > memory0.buffer.byteLength || len6 > Math.floor((memory0.buffer.byteLength - result6) / 40)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			for (let i = 0; i < vec6.length; i++) {
				let e = vec6[i];
				let base = result6 + i * 40;
				var { address: v0_0, name: v0_1, rssi: v0_2, data: v0_3, scanResponse: v0_4 } = e;
				var val1 = v0_0;
				var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
				var ptr1 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len1 * 1);
				ptr1 >>>= 0;
				if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData1;
				let valLenBytes1 = len1 * 1;
				if (Array.isArray(val1)) {
					let offset = 0;
					let dv1 = new DataView(memory0.buffer);
					for (let v of val1) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv1.setUint8(ptr1 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
					new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, len1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 0, ptr1, !0);
				var variant3 = v0_1;
				if (variant3 == null) dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 8, 0, !0);
				else {
					let e = variant3;
					dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 8, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr2 = encodeRes.ptr;
					var len2 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 16, len2, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 12, ptr2, !0);
				}
				dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 20, toInt8(_jcoStrictNumericContext, v0_2), !0);
				var val4 = v0_3;
				var len4 = Array.isArray(val4) ? val4.length : val4.byteLength;
				var ptr4 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len4 * 1);
				ptr4 >>>= 0;
				if (ptr4 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr4 > memory0.buffer.byteLength || len4 > Math.floor((memory0.buffer.byteLength - ptr4) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData4;
				let valLenBytes4 = len4 * 1;
				if (Array.isArray(val4)) {
					let offset = 0;
					let dv4 = new DataView(memory0.buffer);
					for (let v of val4) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv4.setUint8(ptr4 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData4 = new Uint8Array(val4.buffer || val4, val4.byteOffset, valLenBytes4);
					new Uint8Array(memory0.buffer, ptr4, valLenBytes4).set(valData4);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 28, len4, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 24, ptr4, !0);
				var val5 = v0_4;
				var len5 = Array.isArray(val5) ? val5.length : val5.byteLength;
				var ptr5 = ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				})(0, 0, 1, len5 * 1);
				ptr5 >>>= 0;
				if (ptr5 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
				if (ptr5 > memory0.buffer.byteLength || len5 > Math.floor((memory0.buffer.byteLength - ptr5) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
				let valData5;
				let valLenBytes5 = len5 * 1;
				if (Array.isArray(val5)) {
					let offset = 0;
					let dv5 = new DataView(memory0.buffer);
					for (let v of val5) {
						_requireValidNumericPrimitive.bind(null, "u8")(v);
						dv5.setUint8(ptr5 + offset, v, !0);
						offset += 1;
					}
				} else {
					valData5 = new Uint8Array(val5.buffer || val5, val5.byteOffset, valLenBytes5);
					new Uint8Array(memory0.buffer, ptr5, valLenBytes5).set(valData5);
				}
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 36, len5, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 32, ptr5, !0);
			}
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 4, len6, !0);
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 0, result6, !0);
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"ble-scan\"][Instruction::Return]", {
				funcName: "ble-scan",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline23.fnName = "litz:mcu/host@0.2.0#bleScan";
		let _trampoline24 = function(arg0, arg1, arg2, arg3, arg4, arg5) {
			var ptr0 = arg0 >>> 0 >>> 0;
			var len0 = arg1 >>> 0;
			if (ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
			var result0 = _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr0, len0));
			let variant2;
			switch (arg2) {
				case 0:
					variant2 = void 0;
					break;
				case 1:
					var ptr1 = arg3 >>> 0 >>> 0;
					var len1 = arg4 >>> 0;
					if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
					variant2 = _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr1, len1));
					break;
				default: throw TypeError("invalid variant discriminant for option");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"wifi-join\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "wifiJoin",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "result-catch-handler",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				let hostRet3 = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => wifiJoin(result0, variant2))
				});
				ret = typeof hostRet3 == "object" && hostRet3 && (hostRet3.tag === "ok" || hostRet3.tag === "err") ? hostRet3 : {
					tag: "ok",
					val: hostRet3
				};
			} catch (e) {
				if (getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(e)) throw e;
				ret = {
					tag: "err",
					val: getErrorPayload(hasOwnProperty, e)
				};
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var variant9 = ret;
			switch (variant9.tag) {
				case "ok": {
					let e = variant9.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 0, !0);
					var { ip: v4_0, gateway: v4_1, dns: v4_2 } = e;
					var val5 = v4_0;
					var len5 = Array.isArray(val5) ? val5.length : val5.byteLength;
					var ptr5 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len5 * 1);
					ptr5 >>>= 0;
					if (ptr5 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr5 > memory0.buffer.byteLength || len5 > Math.floor((memory0.buffer.byteLength - ptr5) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData5;
					let valLenBytes5 = len5 * 1;
					if (Array.isArray(val5)) {
						let offset = 0;
						let dv5 = new DataView(memory0.buffer);
						for (let v of val5) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv5.setUint8(ptr5 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData5 = new Uint8Array(val5.buffer || val5, val5.byteOffset, valLenBytes5);
						new Uint8Array(memory0.buffer, ptr5, valLenBytes5).set(valData5);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 8, len5, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 4, ptr5, !0);
					var val6 = v4_1;
					var len6 = Array.isArray(val6) ? val6.length : val6.byteLength;
					var ptr6 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len6 * 1);
					ptr6 >>>= 0;
					if (ptr6 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr6 > memory0.buffer.byteLength || len6 > Math.floor((memory0.buffer.byteLength - ptr6) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData6;
					let valLenBytes6 = len6 * 1;
					if (Array.isArray(val6)) {
						let offset = 0;
						let dv6 = new DataView(memory0.buffer);
						for (let v of val6) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv6.setUint8(ptr6 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData6 = new Uint8Array(val6.buffer || val6, val6.byteOffset, valLenBytes6);
						new Uint8Array(memory0.buffer, ptr6, valLenBytes6).set(valData6);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 16, len6, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 12, ptr6, !0);
					var val7 = v4_2;
					var len7 = Array.isArray(val7) ? val7.length : val7.byteLength;
					var ptr7 = ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					})(0, 0, 1, len7 * 1);
					ptr7 >>>= 0;
					if (ptr7 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
					if (ptr7 > memory0.buffer.byteLength || len7 > Math.floor((memory0.buffer.byteLength - ptr7) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
					let valData7;
					let valLenBytes7 = len7 * 1;
					if (Array.isArray(val7)) {
						let offset = 0;
						let dv7 = new DataView(memory0.buffer);
						for (let v of val7) {
							_requireValidNumericPrimitive.bind(null, "u8")(v);
							dv7.setUint8(ptr7 + offset, v, !0);
							offset += 1;
						}
					} else {
						valData7 = new Uint8Array(val7.buffer || val7, val7.byteOffset, valLenBytes7);
						new Uint8Array(memory0.buffer, ptr7, valLenBytes7).set(valData7);
					}
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 24, len7, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 20, ptr7, !0);
					break;
				}
				case "err": {
					let e = variant9.val;
					dataView(_jcoDataViewCacheContext, memory0).setInt8((arg5 >>> 0) + 0, 1, !0);
					var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, e, ((...args) => {
						let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						let previousMayLeave = state.mayLeave;
						let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
						let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
						let previousStorage = thread?.storage;
						thread && (thread.storage = [0, 0]);
						state.mayLeave = !1;
						let restore = () => {
							state.mayLeave = previousMayLeave;
							thread && (thread.storage = previousStorage);
						};
						let result;
						try {
							result = realloc0(...args);
						} catch (err) {
							restore();
							throw err;
						}
						if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
						restore();
						return result;
					}), memory0);
					var ptr8 = encodeRes.ptr;
					var len8 = encodeRes.len;
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 8, len8, !0);
					dataView(_jcoDataViewCacheContext, memory0).setUint32((arg5 >>> 0) + 4, ptr8, !0);
					break;
				}
				default:
					_debugLog("ERROR: invalid value (expected result as object with 'tag' member)", {
						value: variant9,
						valueType: typeof variant9
					});
					throw TypeError("invalid variant specified for result");
			}
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"wifi-join\"][Instruction::Return]", {
				funcName: "wifi-join",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline24.fnName = "litz:mcu/host@0.2.0#wifiJoin";
		let _trampoline25 = function(arg0) {
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"wifi-scan\"] [Instruction::CallInterface] (sync, @ enter)");
			let parentTask;
			let task;
			let threadID;
			let subtask;
			let createTask = () => {
				({task, threadID} = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
					componentIdx: -1,
					isAsync: !1,
					entryFnName: "wifiScan",
					getCallbackFn: () => null,
					callbackFnName: null,
					errHandling: "none",
					callingWasmExport: !1
				}));
			};
			taskCreation: {
				parentTask = getCurrentTask(_jcoTaskLookupContext, 0, _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0)?.taskID)?.task;
				if (!parentTask) {
					createTask();
					break taskCreation;
				}
				createTask();
				subtask = parentTask.getLatestSubtask();
				if (!subtask) throw Error(`Missing subtask (in parent task [${parentTask.id()}]) for host import, has the import been lowered? (ensure asyncImports are set properly)`);
				task.setParentSubtask(subtask);
			}
			task.enterSync();
			let ret;
			try {
				ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
					componentIdx: task.componentIdx(),
					taskID: task.id(),
					threadID,
					fn: () => _trackHostOperation(_jcoComponentHostOperationContext, () => wifiScan())
				});
			} catch (err) {
				_debugLog("[Instruction::CallInterface] error during sync call", {
					taskID: task.id(),
					subtaskID: task.getParentSubtask()?.id(),
					err
				});
				getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
				task.setErrored(err);
				task.reject(err);
				task.exit();
				throw err;
			}
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfInterrupted();
			var vec2 = ret;
			var len2 = vec2.length;
			var result2 = ((...args) => {
				let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
				let previousMayLeave = state.mayLeave;
				let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
				let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
				let previousStorage = thread?.storage;
				thread && (thread.storage = [0, 0]);
				state.mayLeave = !1;
				let restore = () => {
					state.mayLeave = previousMayLeave;
					thread && (thread.storage = previousStorage);
				};
				let result;
				try {
					result = realloc0(...args);
				} catch (err) {
					restore();
					throw err;
				}
				if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
				restore();
				return result;
			})(0, 0, 4, len2 * 12);
			result2 >>>= 0;
			if (result2 % 4 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
			if (result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 12)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
			if (result2 < 0 || len2 < 0 || len2 > 22369621 || result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 12)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
			for (let i = 0; i < vec2.length; i++) {
				let e = vec2[i];
				let base = result2 + i * 12;
				var { ssid: v0_0, channel: v0_1, rssi: v0_2, secure: v0_3, joined: v0_4 } = e;
				var encodeRes = _utf8AllocateAndEncode(TEXT_ENCODER_UTF8, v0_0, ((...args) => {
					let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
					let previousMayLeave = state.mayLeave;
					let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
					let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
					let previousStorage = thread?.storage;
					thread && (thread.storage = [0, 0]);
					state.mayLeave = !1;
					let restore = () => {
						state.mayLeave = previousMayLeave;
						thread && (thread.storage = previousStorage);
					};
					let result;
					try {
						result = realloc0(...args);
					} catch (err) {
						restore();
						throw err;
					}
					if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
					restore();
					return result;
				}), memory0);
				var ptr1 = encodeRes.ptr;
				var len1 = encodeRes.len;
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 4, len1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setUint32(base + 0, ptr1, !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 8, toUint8(_jcoStrictNumericContext, v0_1), !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 9, toInt8(_jcoStrictNumericContext, v0_2), !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 10, +!!v0_3, !0);
				dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 11, +!!v0_4, !0);
			}
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 4, len2, !0);
			dataView(_jcoDataViewCacheContext, memory0).setUint32((arg0 >>> 0) + 0, result2, !0);
			_debugLog("[iface=\"litz:mcu/host@0.2.0\", function=\"wifi-scan\"][Instruction::Return]", {
				funcName: "wifi-scan",
				paramCount: 0,
				async: !1,
				postReturn: !1
			});
			task.resolve([ret]);
			task.exit();
		};
		_trampoline25.fnName = "litz:mcu/host@0.2.0#wifiScan";
		let exports2;
		let postReturn0;
		let postReturn1;
		let postReturn2;
		let postReturn3;
		let chip020Info;
		function info() {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020Info",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "none",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(31);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"info\"][Instruction::CallWasm] enter", {
							funcName: "info",
							paramCount: 0,
							async: !1,
							postReturn: !0
						});
						let ret;
						try {
							ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020Info()
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						var ptr0 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 0, !0) >>> 0;
						var len0 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 4, !0) >>> 0;
						if (ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
						var result0 = _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr0, len0));
						var ptr1 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 8, !0) >>> 0;
						var len1 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 12, !0) >>> 0;
						if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
						var result1 = _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr1, len1));
						var len3 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 28, !0);
						var base3 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 24, !0) >>> 0;
						if (base3 < 0 || len3 < 0 || len3 > 16777215 || base3 > memory0.buffer.byteLength || len3 > Math.floor((memory0.buffer.byteLength - base3) / 16)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
						if (base3 % 4 != 0) throw TypeError(`list pointer [${base3}] is not aligned to 4`);
						var result3 = [];
						for (let i = 0; i < len3; i++) {
							let base = base3 + i * 16;
							var ptr2 = dataView(_jcoDataViewCacheContext, memory0).getUint32(base + 0, !0) >>> 0;
							var len2 = dataView(_jcoDataViewCacheContext, memory0).getUint32(base + 4, !0) >>> 0;
							if (ptr2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - ptr2) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
							var result2 = _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr2, len2));
							result3.push({
								name: result2,
								start: dataView(_jcoDataViewCacheContext, memory0).getInt32(base + 8, !0) >>> 0,
								len: dataView(_jcoDataViewCacheContext, memory0).getInt32(base + 12, !0) >>> 0
							});
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"info\"][Instruction::Return]", {
							funcName: "info",
							paramCount: 1,
							async: !1,
							postReturn: !0
						});
						task.resolve([{
							name: result0,
							description: result1,
							cpuHz: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 16, !0))),
							memory: result3,
							flash: {
								size: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 32, !0))),
								bootloader: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 40, !0))),
								partitionTable: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 48, !0))),
								app: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 56, !0)))
							}
						}]);
						let retCopy = {
							name: result0,
							description: result1,
							cpuHz: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 16, !0))),
							memory: result3,
							flash: {
								size: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 32, !0))),
								bootloader: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 40, !0))),
								partitionTable: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 48, !0))),
								app: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 56, !0)))
							}
						};
						let cstate = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						cstate.mayLeave = !1;
						try {
							postReturn0(ret);
						} catch (err) {
							err = _postReturnTrap(_jcoComponentPostReturnTrapContext, 0, err);
							task.reject(err);
							throw err;
						}
						cstate.mayLeave = !0;
						task.exit();
						return retCopy;
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020LoadImage;
		function loadImage(arg0, arg1, arg2) {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020LoadImage",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "throw-result-err",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(19);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						var val0 = arg0;
						let enum0;
						switch (val0) {
							case "rom":
								enum0 = 0;
								break;
							case "bootloader":
								enum0 = 1;
								break;
							case "partition-table":
								enum0 = 2;
								break;
							case "firmware":
								enum0 = 3;
								break;
							default:
								arg0 instanceof Error && console.error(arg0);
								throw TypeError(`"${val0}" is not one of the cases of image`);
						}
						var val1 = arg2;
						var len1 = Array.isArray(val1) ? val1.length : val1.byteLength;
						var ptr1 = ((...args) => {
							let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
							let previousMayLeave = state.mayLeave;
							let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
							let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
							let previousStorage = thread?.storage;
							thread && (thread.storage = [0, 0]);
							state.mayLeave = !1;
							let restore = () => {
								state.mayLeave = previousMayLeave;
								thread && (thread.storage = previousStorage);
							};
							let result;
							try {
								result = realloc0(...args);
							} catch (err) {
								restore();
								throw err;
							}
							if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
							restore();
							return result;
						})(0, 0, 1, len1 * 1);
						ptr1 >>>= 0;
						if (ptr1 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
						if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
						let valData1;
						let valLenBytes1 = len1 * 1;
						if (Array.isArray(val1)) {
							let offset = 0;
							let dv1 = new DataView(memory0.buffer);
							for (let v of val1) {
								_requireValidNumericPrimitive.bind(null, "u8")(v);
								dv1.setUint8(ptr1 + offset, v, !0);
								offset += 1;
							}
						} else {
							valData1 = new Uint8Array(val1.buffer || val1, val1.byteOffset, valLenBytes1);
							new Uint8Array(memory0.buffer, ptr1, valLenBytes1).set(valData1);
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"load-image\"][Instruction::CallWasm] enter", {
							funcName: "load-image",
							paramCount: 4,
							async: !1,
							postReturn: !0
						});
						let ret;
						try {
							ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020LoadImage(enum0, toUint64(_jcoStrictNumericContext, arg1), ptr1, len1)
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						let variant3;
						switch (dataView(_jcoDataViewCacheContext, memory0).getUint8((ret >>> 0) + 0, !0)) {
							case 0:
								variant3 = {
									tag: "ok",
									val: void 0
								};
								break;
							case 1:
								var ptr2 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 4, !0) >>> 0;
								var len2 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 8, !0) >>> 0;
								if (ptr2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - ptr2) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
								variant3 = {
									tag: "err",
									val: _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr2, len2))
								};
								break;
							default: throw TypeError("invalid variant discriminant for expected");
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"load-image\"][Instruction::Return]", {
							funcName: "load-image",
							paramCount: 1,
							async: !1,
							postReturn: !0
						});
						let retCopy = variant3;
						task.resolve([retCopy]);
						let cstate = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						cstate.mayLeave = !1;
						try {
							postReturn1(ret);
						} catch (err) {
							err = _postReturnTrap(_jcoComponentPostReturnTrapContext, 0, err);
							task.reject(err);
							throw err;
						}
						cstate.mayLeave = !0;
						task.exit();
						if (typeof retCopy == "object" && retCopy.tag === "err") throw new ComponentError(retCopy.val);
						return retCopy.val;
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020Reset;
		function reset(arg0) {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020Reset",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "none",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(1);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						var val0 = arg0;
						var len0 = Array.isArray(val0) ? val0.length : val0.byteLength;
						var ptr0 = ((...args) => {
							let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
							let previousMayLeave = state.mayLeave;
							let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
							let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
							let previousStorage = thread?.storage;
							thread && (thread.storage = [0, 0]);
							state.mayLeave = !1;
							let restore = () => {
								state.mayLeave = previousMayLeave;
								thread && (thread.storage = previousStorage);
							};
							let result;
							try {
								result = realloc0(...args);
							} catch (err) {
								restore();
								throw err;
							}
							if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
							restore();
							return result;
						})(0, 0, 1, len0 * 1);
						ptr0 >>>= 0;
						if (ptr0 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
						if (ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
						let valData0;
						let valLenBytes0 = len0 * 1;
						if (Array.isArray(val0)) {
							let offset = 0;
							let dv0 = new DataView(memory0.buffer);
							for (let v of val0) {
								_requireValidNumericPrimitive.bind(null, "u8")(v);
								dv0.setUint8(ptr0 + offset, v, !0);
								offset += 1;
							}
						} else {
							valData0 = new Uint8Array(val0.buffer || val0, val0.byteOffset, valLenBytes0);
							new Uint8Array(memory0.buffer, ptr0, valLenBytes0).set(valData0);
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"reset\"][Instruction::CallWasm] enter", {
							funcName: "reset",
							paramCount: 2,
							async: !1,
							postReturn: !1
						});
						let ret;
						try {
							_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020Reset(ptr0, len0)
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"reset\"][Instruction::Return]", {
							funcName: "reset",
							paramCount: 0,
							async: !1,
							postReturn: !1
						});
						task.resolve([ret]);
						task.exit();
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020Boot;
		function boot() {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020Boot",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "throw-result-err",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(12);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"boot\"][Instruction::CallWasm] enter", {
							funcName: "boot",
							paramCount: 0,
							async: !1,
							postReturn: !0
						});
						let ret;
						try {
							ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020Boot()
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						let variant1;
						switch (dataView(_jcoDataViewCacheContext, memory0).getUint8((ret >>> 0) + 0, !0)) {
							case 0:
								variant1 = {
									tag: "ok",
									val: dataView(_jcoDataViewCacheContext, memory0).getInt32((ret >>> 0) + 4, !0) >>> 0
								};
								break;
							case 1:
								var ptr0 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 4, !0) >>> 0;
								var len0 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 8, !0) >>> 0;
								if (ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
								variant1 = {
									tag: "err",
									val: _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr0, len0))
								};
								break;
							default: throw TypeError("invalid variant discriminant for expected");
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"boot\"][Instruction::Return]", {
							funcName: "boot",
							paramCount: 1,
							async: !1,
							postReturn: !0
						});
						let retCopy = variant1;
						task.resolve([retCopy]);
						let cstate = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						cstate.mayLeave = !1;
						try {
							postReturn1(ret);
						} catch (err) {
							err = _postReturnTrap(_jcoComponentPostReturnTrapContext, 0, err);
							task.reject(err);
							throw err;
						}
						cstate.mayLeave = !0;
						task.exit();
						if (typeof retCopy == "object" && retCopy.tag === "err") throw new ComponentError(retCopy.val);
						return retCopy.val;
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020SetInputs;
		function setInputs(arg0) {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020SetInputs",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "none",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(1);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						var vec2 = arg0;
						var len2 = vec2.length;
						var result2 = ((...args) => {
							let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
							let previousMayLeave = state.mayLeave;
							let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
							let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
							let previousStorage = thread?.storage;
							thread && (thread.storage = [0, 0]);
							state.mayLeave = !1;
							let restore = () => {
								state.mayLeave = previousMayLeave;
								thread && (thread.storage = previousStorage);
							};
							let result;
							try {
								result = realloc0(...args);
							} catch (err) {
								restore();
								throw err;
							}
							if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
							restore();
							return result;
						})(0, 0, 1, len2 * 2);
						result2 >>>= 0;
						if (result2 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
						if (result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 2)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
						if (result2 < 0 || len2 < 0 || len2 > 134217727 || result2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - result2) / 2)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
						for (let i = 0; i < vec2.length; i++) {
							let e = vec2[i];
							let base = result2 + i * 2;
							var { pin: v0_0, level: v0_1 } = e;
							dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 0, toUint8(_jcoStrictNumericContext, v0_0), !0);
							var val1 = v0_1;
							let enum1;
							switch (val1) {
								case "low":
									enum1 = 0;
									break;
								case "high":
									enum1 = 1;
									break;
								case "floating":
									enum1 = 2;
									break;
								default:
									v0_1 instanceof Error && console.error(v0_1);
									throw TypeError(`"${val1}" is not one of the cases of level`);
							}
							dataView(_jcoDataViewCacheContext, memory0).setInt8(base + 1, enum1, !0);
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"set-inputs\"][Instruction::CallWasm] enter", {
							funcName: "set-inputs",
							paramCount: 2,
							async: !1,
							postReturn: !1
						});
						let ret;
						try {
							_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020SetInputs(result2, len2)
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"set-inputs\"][Instruction::Return]", {
							funcName: "set-inputs",
							paramCount: 0,
							async: !1,
							postReturn: !1
						});
						task.resolve([ret]);
						task.exit();
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020Run;
		function run(arg0) {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020Run",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "none",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(35);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"run\"][Instruction::CallWasm] enter", {
							funcName: "run",
							paramCount: 1,
							async: !1,
							postReturn: !1
						});
						let ret;
						try {
							ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020Run(toUint64(_jcoStrictNumericContext, arg0))
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						let enum0;
						switch (dataView(_jcoDataViewCacheContext, memory0).getUint8((ret >>> 0) + 8, !0)) {
							case 0:
								enum0 = "budget";
								break;
							case 1:
								enum0 = "idle";
								break;
							case 2:
								enum0 = "halted";
								break;
							case 3:
								enum0 = "trap";
								break;
							case 4:
								enum0 = "reset";
								break;
							default: throw TypeError("invalid discriminant specified for Stop");
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"run\"][Instruction::Return]", {
							funcName: "run",
							paramCount: 1,
							async: !1,
							postReturn: !1
						});
						task.resolve([{
							cycles: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 0, !0))),
							stop: enum0
						}]);
						task.exit();
						return {
							cycles: BigInt.asUintN(64, BigInt(dataView(_jcoDataViewCacheContext, memory0).getBigInt64((ret >>> 0) + 0, !0))),
							stop: enum0
						};
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020TakeOutputs;
		function takeOutputs() {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020TakeOutputs",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "none",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(36);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"take-outputs\"][Instruction::CallWasm] enter", {
							funcName: "take-outputs",
							paramCount: 0,
							async: !1,
							postReturn: !0
						});
						let ret;
						try {
							ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020TakeOutputs()
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						var len2 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 4, !0);
						var base2 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 0, !0) >>> 0;
						if (base2 < 0 || len2 < 0 || len2 > 89478485 || base2 > memory0.buffer.byteLength || len2 > Math.floor((memory0.buffer.byteLength - base2) / 3)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
						if (base2 % 1 != 0) throw TypeError(`list pointer [${base2}] is not aligned to 1`);
						var result2 = [];
						for (let i = 0; i < len2; i++) {
							let base = base2 + i * 3;
							let enum0;
							switch (dataView(_jcoDataViewCacheContext, memory0).getUint8(base + 1, !0)) {
								case 0:
									enum0 = "low";
									break;
								case 1:
									enum0 = "high";
									break;
								case 2:
									enum0 = "floating";
									break;
								default: throw TypeError("invalid discriminant specified for Level");
							}
							var bool1 = dataView(_jcoDataViewCacheContext, memory0).getUint8(base + 2, !0);
							result2.push({
								pin: clampGuest(dataView(_jcoDataViewCacheContext, memory0).getUint8(base + 0, !0), 0, 255),
								level: enum0,
								enabled: bool1 == 0 ? !1 : bool1 == 1 || throwInvalidBool()
							});
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"take-outputs\"][Instruction::Return]", {
							funcName: "take-outputs",
							paramCount: 1,
							async: !1,
							postReturn: !0
						});
						task.resolve([result2]);
						let retCopy = result2;
						let cstate = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						cstate.mayLeave = !1;
						try {
							postReturn2(ret);
						} catch (err) {
							err = _postReturnTrap(_jcoComponentPostReturnTrapContext, 0, err);
							task.reject(err);
							throw err;
						}
						cstate.mayLeave = !0;
						task.exit();
						return retCopy;
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020WriteConsole;
		function writeConsole(arg0) {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020WriteConsole",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "none",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(1);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						var val0 = arg0;
						var len0 = Array.isArray(val0) ? val0.length : val0.byteLength;
						var ptr0 = ((...args) => {
							let state = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
							let previousMayLeave = state.mayLeave;
							let meta = _getGlobalCurrentThreadMeta(CURRENT_TASK_META, 0);
							let thread = meta && getCurrentThread(_jcoThreadLookupContext, meta.componentIdx, meta.threadID, meta.taskID)?.thread;
							let previousStorage = thread?.storage;
							thread && (thread.storage = [0, 0]);
							state.mayLeave = !1;
							let restore = () => {
								state.mayLeave = previousMayLeave;
								thread && (thread.storage = previousStorage);
							};
							let result;
							try {
								result = realloc0(...args);
							} catch (err) {
								restore();
								throw err;
							}
							if (result && typeof result.then == "function") return Promise.resolve(result).finally(restore);
							restore();
							return result;
						})(0, 0, 1, len0 * 1);
						ptr0 >>>= 0;
						if (ptr0 % 1 != 0) throw new WebAssemblyRuntimeError("realloc return: result not aligned");
						if (ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("realloc return: beyond end of memory");
						let valData0;
						let valLenBytes0 = len0 * 1;
						if (Array.isArray(val0)) {
							let offset = 0;
							let dv0 = new DataView(memory0.buffer);
							for (let v of val0) {
								_requireValidNumericPrimitive.bind(null, "u8")(v);
								dv0.setUint8(ptr0 + offset, v, !0);
								offset += 1;
							}
						} else {
							valData0 = new Uint8Array(val0.buffer || val0, val0.byteOffset, valLenBytes0);
							new Uint8Array(memory0.buffer, ptr0, valLenBytes0).set(valData0);
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"write-console\"][Instruction::CallWasm] enter", {
							funcName: "write-console",
							paramCount: 2,
							async: !1,
							postReturn: !1
						});
						let ret;
						try {
							_withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020WriteConsole(ptr0, len0)
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"write-console\"][Instruction::Return]", {
							funcName: "write-console",
							paramCount: 0,
							async: !1,
							postReturn: !1
						});
						task.resolve([ret]);
						task.exit();
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let chip020Read;
		function read(arg0, arg1) {
			getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).throwIfTrapped();
			let { task, threadID } = createNewCurrentImplicitThread(_jcoTaskCreateContext, {
				componentIdx: 0,
				isAsync: !1,
				isManualAsync: !1,
				preserveFutureResult: !1,
				entryFnName: "chip020Read",
				getCallbackFn: () => null,
				callbackFnName: null,
				errHandling: "throw-result-err",
				callingWasmExport: !0
			});
			task.setCalleeIsAsync(!1);
			task.setFuncTypeIsAsync(!1);
			task.setReturnResultTypeIdx(5);
			task.enterSync();
			CURRENT_TASK_MAY_BLOCK.value = +!!task.mayBlock();
			task.setReturnMemoryIdx(0);
			task.setReturnMemory(memory0);
			return _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
				taskID: task.id(),
				threadID,
				componentIdx: task.componentIdx(),
				fn: () => {
					try {
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"read\"][Instruction::CallWasm] enter", {
							funcName: "read",
							paramCount: 2,
							async: !1,
							postReturn: !0
						});
						let ret;
						try {
							ret = _withGlobalCurrentThreadMeta(CURRENT_TASK_META, {
								taskID: task.id(),
								threadID,
								componentIdx: task.componentIdx(),
								fn: () => chip020Read(toUint32(_jcoStrictNumericContext, arg0), toUint32(_jcoStrictNumericContext, arg1))
							});
						} catch (err) {
							_debugLog("[Instruction::CallWasm] error during sync call", {
								taskID: task.id(),
								err
							});
							getOrCreateAsyncState(_jcoComponentStateLookupContext, 0).markTrapped(err);
							task.setErrored(err);
							task.reject(err);
							task.exit();
							throw err;
						}
						task.endFirstSlice();
						let variant2;
						switch (dataView(_jcoDataViewCacheContext, memory0).getUint8((ret >>> 0) + 0, !0)) {
							case 0:
								var ptr0 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 4, !0) >>> 0;
								var len0 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 8, !0);
								if (ptr0 < 0 || len0 < 0 || len0 > 268435455 || ptr0 > memory0.buffer.byteLength || len0 > Math.floor((memory0.buffer.byteLength - ptr0) / 1)) throw new WebAssemblyRuntimeError("wasm trap: out of bounds memory access");
								if (ptr0 % 1 != 0) throw TypeError(`list pointer [${ptr0}] is not aligned to 1`);
								variant2 = {
									tag: "ok",
									val: new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1))
								};
								break;
							case 1:
								var ptr1 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 4, !0) >>> 0;
								var len1 = dataView(_jcoDataViewCacheContext, memory0).getUint32((ret >>> 0) + 8, !0) >>> 0;
								if (ptr1 > memory0.buffer.byteLength || len1 > Math.floor((memory0.buffer.byteLength - ptr1) / 1)) throw new WebAssemblyRuntimeError("string pointer/length out of bounds of memory");
								variant2 = {
									tag: "err",
									val: _decodeUtf8(WebAssemblyRuntimeError, TEXT_DECODER_UTF8, new Uint8Array(memory0.buffer, ptr1, len1))
								};
								break;
							default: throw TypeError("invalid variant discriminant for expected");
						}
						_debugLog("[iface=\"litz:mcu/chip@0.2.0\", function=\"read\"][Instruction::Return]", {
							funcName: "read",
							paramCount: 1,
							async: !1,
							postReturn: !0
						});
						let retCopy = variant2;
						task.resolve([retCopy]);
						let cstate = getOrCreateAsyncState(_jcoComponentStateLookupContext, 0);
						cstate.mayLeave = !1;
						try {
							postReturn3(ret);
						} catch (err) {
							err = _postReturnTrap(_jcoComponentPostReturnTrapContext, 0, err);
							task.reject(err);
							throw err;
						}
						cstate.mayLeave = !0;
						task.exit();
						if (typeof retCopy == "object" && retCopy.tag === "err") throw new ComponentError(retCopy.val);
						return retCopy.val;
					} catch (err) {
						if (!task.isResolvedState()) {
							task.setErrored(err);
							task.reject(err);
						}
						task.isExited() || task.exit({ skipExclusiveLockCheck: !0 });
						throw err;
					}
				}
			});
		}
		let trampoline0 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 0,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline0.manuallyAsync,
			paramLiftFns: [_liftFlatU16],
			resultLowerFns: [],
			hasResultPointer: !1,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: null,
			stringEncoding: "utf8",
			getMemoryFn: () => null,
			getReallocFn: void 0,
			importFn: _trampoline0
		});
		let trampoline1 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 1,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline1.manuallyAsync,
			paramLiftFns: [_liftFlatU32],
			resultLowerFns: [],
			hasResultPointer: !1,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: null,
			stringEncoding: "utf8",
			getMemoryFn: () => null,
			getReallocFn: void 0,
			importFn: _trampoline1
		});
		let trampoline2 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 2,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline2.manuallyAsync,
			paramLiftFns: [_liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatU8,
				elemAlign32: 1,
				elemSize32: 1,
				typedArray: Uint8Array
			})],
			resultLowerFns: [],
			hasResultPointer: !1,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: void 0,
			importFn: _trampoline2
		});
		let trampoline3 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 3,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline3.manuallyAsync,
			paramLiftFns: [
				_liftFlatU8,
				_liftFlatU8,
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				}),
				_liftFlatU32
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatList(_jcoLowerListContext, {
						elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
						elemSize32: 1,
						elemAlign32: 1,
						typedArray: Uint8Array
					}),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline3
		});
		let trampoline4 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 4,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline4.manuallyAsync,
			paramLiftFns: [
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				}),
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				}),
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				})
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatList(_jcoLowerListContext, {
						elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
						elemSize32: 1,
						elemAlign32: 1,
						typedArray: Uint8Array
					}),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline4
		});
		let trampoline5 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 5,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline5.manuallyAsync,
			paramLiftFns: [
				_liftFlatStringAny.bind(void 0, _jcoLiftStringContext),
				_liftFlatU16,
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				})
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					null,
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline5
		});
		let trampoline6 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 6,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline6.manuallyAsync,
			paramLiftFns: [_liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatU8,
				elemAlign32: 1,
				elemSize32: 1,
				typedArray: Uint8Array
			})],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline6
		});
		let trampoline7 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 7,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline7.manuallyAsync,
			paramLiftFns: [_liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatU8,
				elemAlign32: 1,
				elemSize32: 1,
				typedArray: Uint8Array
			})],
			resultLowerFns: [],
			hasResultPointer: !1,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: void 0,
			importFn: _trampoline7
		});
		let trampoline8 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 8,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline8.manuallyAsync,
			paramLiftFns: [_liftFlatU8, _liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatU8,
				elemAlign32: 1,
				elemSize32: 1,
				typedArray: Uint8Array
			})],
			resultLowerFns: [_lowerFlatBool.bind(void 0, _jcoStrictNumericContext)],
			hasResultPointer: !1,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: void 0,
			importFn: _trampoline8
		});
		let trampoline9 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 9,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline9.manuallyAsync,
			paramLiftFns: [
				_liftFlatU16,
				_liftFlatU16,
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				})
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					null,
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline9
		});
		let trampoline10 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 10,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline10.manuallyAsync,
			paramLiftFns: [
				_liftFlatU16,
				_liftFlatU16,
				_liftFlatU16,
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				})
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					null,
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline10
		});
		let trampoline11 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 11,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline11.manuallyAsync,
			paramLiftFns: [_liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatU8,
				elemAlign32: 1,
				elemSize32: 1,
				typedArray: Uint8Array
			}), _liftFlatU16],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatU32.bind(void 0, _jcoStrictNumericContext),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline11
		});
		let trampoline12 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 12,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline12.manuallyAsync,
			paramLiftFns: [_liftFlatU32, _liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatU8,
				elemAlign32: 1,
				elemSize32: 1,
				typedArray: Uint8Array
			})],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatU32.bind(void 0, _jcoStrictNumericContext),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline12
		});
		let trampoline13 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 13,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline13.manuallyAsync,
			paramLiftFns: [_liftFlatU32, _liftFlatU32],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatList(_jcoLowerListContext, {
						elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
						elemSize32: 1,
						elemAlign32: 1,
						typedArray: Uint8Array
					}),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline13
		});
		let trampoline14 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 14,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline14.manuallyAsync,
			paramLiftFns: [
				_liftFlatList(_jcoLiftListContext, {
					elemLiftFn: _liftFlatU8,
					elemAlign32: 1,
					elemSize32: 1,
					typedArray: Uint8Array
				}),
				_liftFlatOption(_jcoLiftVariantContext, {
					caseMetas: [[
						"none",
						null,
						0,
						0,
						0,
						[]
					], [
						"some",
						_liftFlatList(_jcoLiftListContext, {
							elemLiftFn: _liftFlatU8,
							elemAlign32: 1,
							elemSize32: 1,
							typedArray: Uint8Array
						}),
						8,
						4,
						2,
						["i32", "i32"]
					]],
					variantSize32: 12,
					variantAlign32: 4,
					variantPayloadOffset32: 4,
					variantFlatCount: 3,
					variantPayloadFlatTypes: ["i32", "i32"],
					payloadMaybeNull: !1
				}),
				_liftFlatU64
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					null,
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline14
		});
		let trampoline15 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 15,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline15.manuallyAsync,
			paramLiftFns: [_liftFlatU8],
			resultLowerFns: [_lowerFlatOption(_jcoStrictNumericContext, {
				caseMetas: [[
					"none",
					null,
					0,
					0,
					0
				], [
					"some",
					_lowerFlatU32.bind(void 0, _jcoStrictNumericContext),
					4,
					4,
					1
				]],
				variantSize32: 8,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 2,
				payloadMaybeNull: !1
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: void 0,
			importFn: _trampoline15
		});
		let trampoline16 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 16,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline16.manuallyAsync,
			paramLiftFns: [_liftFlatU16],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatList(_jcoLowerListContext, {
						elemLowerFn: _lowerFlatRecord({
							fieldMetas: [[
								"uuid",
								_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
								2,
								2
							], [
								"characteristics",
								_lowerFlatList(_jcoLowerListContext, {
									elemLowerFn: _lowerFlatRecord({
										fieldMetas: [
											[
												"uuid",
												_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
												2,
												2
											],
											[
												"access",
												_lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
												1,
												1
											],
											[
												"value",
												_lowerFlatList(_jcoLowerListContext, {
													elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
													elemSize32: 1,
													elemAlign32: 1,
													typedArray: Uint8Array
												}),
												8,
												4
											]
										],
										size32: 12,
										align32: 4
									}),
									elemSize32: 12,
									elemAlign32: 4,
									typedArray: void 0
								}),
								8,
								4
							]],
							size32: 12,
							align32: 4
						}),
						elemSize32: 12,
						elemAlign32: 4,
						typedArray: void 0
					}),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline16
		});
		let trampoline17 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 17,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline17.manuallyAsync,
			paramLiftFns: [_liftFlatList(_jcoLiftListContext, {
				elemLiftFn: _liftFlatRecord({
					fieldMetas: [[
						"uuid",
						_liftFlatU16,
						2,
						2
					], [
						"characteristics",
						_liftFlatList(_jcoLiftListContext, {
							elemLiftFn: _liftFlatRecord({
								fieldMetas: [
									[
										"uuid",
										_liftFlatU16,
										2,
										2
									],
									[
										"access",
										_liftFlatU8,
										1,
										1
									],
									[
										"value",
										_liftFlatList(_jcoLiftListContext, {
											elemLiftFn: _liftFlatU8,
											elemAlign32: 1,
											elemSize32: 1,
											typedArray: Uint8Array
										}),
										8,
										4
									]
								],
								size32: 12,
								align32: 4
							}),
							elemAlign32: 4,
							elemSize32: 12,
							typedArray: void 0
						}),
						8,
						4
					]],
					size32: 12,
					align32: 4
				}),
				elemAlign32: 4,
				elemSize32: 12,
				typedArray: void 0
			})],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					null,
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline17
		});
		let trampoline18 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 18,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline18.manuallyAsync,
			paramLiftFns: [
				_liftFlatU16,
				_liftFlatU16,
				_liftFlatU16,
				_liftFlatBool
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					null,
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline18
		});
		let trampoline19 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 19,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline19.manuallyAsync,
			paramLiftFns: [],
			resultLowerFns: [_lowerFlatList(_jcoLowerListContext, {
				elemLowerFn: _lowerFlatRecord({
					fieldMetas: [
						[
							"sender",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						],
						[
							"to",
							_lowerFlatOption(_jcoStrictNumericContext, {
								caseMetas: [[
									"none",
									null,
									0,
									0,
									0
								], [
									"some",
									_lowerFlatList(_jcoLowerListContext, {
										elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
										elemSize32: 1,
										elemAlign32: 1,
										typedArray: Uint8Array
									}),
									8,
									4,
									2
								]],
								variantSize32: 12,
								variantAlign32: 4,
								variantPayloadOffset32: 4,
								variantFlatCount: 3,
								payloadMaybeNull: !1
							}),
							12,
							4
						],
						[
							"bytes",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						],
						[
							"sentAt",
							_lowerFlatU64.bind(void 0, _jcoStrictNumericContext),
							8,
							8
						],
						[
							"airtime",
							_lowerFlatU64.bind(void 0, _jcoStrictNumericContext),
							8,
							8
						]
					],
					size32: 48,
					align32: 8
				}),
				elemSize32: 48,
				elemAlign32: 8,
				typedArray: void 0
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline19
		});
		let trampoline20 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 20,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline20.manuallyAsync,
			paramLiftFns: [],
			resultLowerFns: [_lowerFlatList(_jcoLowerListContext, {
				elemLowerFn: _lowerFlatRecord({
					fieldMetas: [
						[
							"service",
							_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
							2,
							2
						],
						[
							"characteristic",
							_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
							2,
							2
						],
						[
							"value",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						]
					],
					size32: 12,
					align32: 4
				}),
				elemSize32: 12,
				elemAlign32: 4,
				typedArray: void 0
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline20
		});
		let trampoline21 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 21,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline21.manuallyAsync,
			paramLiftFns: [_liftFlatU16],
			resultLowerFns: [_lowerFlatList(_jcoLowerListContext, {
				elemLowerFn: _lowerFlatRecord({
					fieldMetas: [
						[
							"service",
							_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
							2,
							2
						],
						[
							"characteristic",
							_lowerFlatU16.bind(void 0, _jcoStrictNumericContext),
							2,
							2
						],
						[
							"value",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						]
					],
					size32: 12,
					align32: 4
				}),
				elemSize32: 12,
				elemAlign32: 4,
				typedArray: void 0
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline21
		});
		let trampoline22 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 22,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline22.manuallyAsync,
			paramLiftFns: [
				_liftFlatU16,
				_liftFlatU16,
				_liftFlatU16
			],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatList(_jcoLowerListContext, {
						elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
						elemSize32: 1,
						elemAlign32: 1,
						typedArray: Uint8Array
					}),
					12,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					12,
					4,
					4
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline22
		});
		let trampoline23 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 23,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline23.manuallyAsync,
			paramLiftFns: [],
			resultLowerFns: [_lowerFlatList(_jcoLowerListContext, {
				elemLowerFn: _lowerFlatRecord({
					fieldMetas: [
						[
							"address",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						],
						[
							"name",
							_lowerFlatOption(_jcoStrictNumericContext, {
								caseMetas: [[
									"none",
									null,
									0,
									0,
									0
								], [
									"some",
									_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
									8,
									4,
									2
								]],
								variantSize32: 12,
								variantAlign32: 4,
								variantPayloadOffset32: 4,
								variantFlatCount: 3,
								payloadMaybeNull: !1
							}),
							12,
							4
						],
						[
							"rssi",
							_lowerFlatS8.bind(void 0, _jcoStrictNumericContext),
							1,
							1
						],
						[
							"data",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						],
						[
							"scanResponse",
							_lowerFlatList(_jcoLowerListContext, {
								elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
								elemSize32: 1,
								elemAlign32: 1,
								typedArray: Uint8Array
							}),
							8,
							4
						]
					],
					size32: 40,
					align32: 4
				}),
				elemSize32: 40,
				elemAlign32: 4,
				typedArray: void 0
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline23
		});
		let trampoline24 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 24,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline24.manuallyAsync,
			paramLiftFns: [_liftFlatStringAny.bind(void 0, _jcoLiftStringContext), _liftFlatOption(_jcoLiftVariantContext, {
				caseMetas: [[
					"none",
					null,
					0,
					0,
					0,
					[]
				], [
					"some",
					_liftFlatStringAny.bind(void 0, _jcoLiftStringContext),
					8,
					4,
					2,
					["i32", "i32"]
				]],
				variantSize32: 12,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 3,
				variantPayloadFlatTypes: ["i32", "i32"],
				payloadMaybeNull: !1
			})],
			resultLowerFns: [_lowerFlatResult(_jcoStrictNumericContext, {
				caseMetas: [[
					"ok",
					_lowerFlatRecord({
						fieldMetas: [
							[
								"ip",
								_lowerFlatList(_jcoLowerListContext, {
									elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
									elemSize32: 1,
									elemAlign32: 1,
									typedArray: Uint8Array
								}),
								8,
								4
							],
							[
								"gateway",
								_lowerFlatList(_jcoLowerListContext, {
									elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
									elemSize32: 1,
									elemAlign32: 1,
									typedArray: Uint8Array
								}),
								8,
								4
							],
							[
								"dns",
								_lowerFlatList(_jcoLowerListContext, {
									elemLowerFn: _lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
									elemSize32: 1,
									elemAlign32: 1,
									typedArray: Uint8Array
								}),
								8,
								4
							]
						],
						size32: 24,
						align32: 4
					}),
					28,
					4,
					4
				], [
					"err",
					_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
					28,
					4,
					4
				]],
				variantSize32: 28,
				variantAlign32: 4,
				variantPayloadOffset32: 4,
				variantFlatCount: 7
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline24
		});
		let trampoline25 = _lowerImportBackwardsCompat.bind(null, _jcoTaskCompatImportContext, {
			trampolineIdx: 25,
			componentIdx: 0,
			isAsync: !1,
			isManualAsync: _trampoline25.manuallyAsync,
			paramLiftFns: [],
			resultLowerFns: [_lowerFlatList(_jcoLowerListContext, {
				elemLowerFn: _lowerFlatRecord({
					fieldMetas: [
						[
							"ssid",
							_lowerFlatStringAny.bind(void 0, _jcoLowerStringContext),
							8,
							4
						],
						[
							"channel",
							_lowerFlatU8.bind(void 0, _jcoStrictNumericContext),
							1,
							1
						],
						[
							"rssi",
							_lowerFlatS8.bind(void 0, _jcoStrictNumericContext),
							1,
							1
						],
						[
							"secure",
							_lowerFlatBool.bind(void 0, _jcoStrictNumericContext),
							1,
							1
						],
						[
							"joined",
							_lowerFlatBool.bind(void 0, _jcoStrictNumericContext),
							1,
							1
						]
					],
					size32: 12,
					align32: 4
				}),
				elemSize32: 12,
				elemAlign32: 4,
				typedArray: void 0
			})],
			hasResultPointer: !0,
			funcTypeIsAsync: !1,
			getCallbackFn: () => null,
			getPostReturnFn: () => null,
			isCancellable: !1,
			memoryIdx: 0,
			stringEncoding: "utf8",
			getMemoryFn: () => memory0,
			getReallocFn: () => realloc0,
			importFn: _trampoline25
		});
		({exports: exports0} = instantiateCore(module1));
		({exports: exports1} = instantiateCore(module0, { "litz:mcu/host@0.2.0": {
			"analog-read": Object.assign(exports0[13], { _jcoMaySuspend: !1 }),
			"ble-advertise": Object.assign(exports0[2], { _jcoMaySuspend: !1 }),
			"ble-connect": Object.assign(exports0[4], { _jcoMaySuspend: !1 }),
			"ble-disconnect": Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline0, !1), { _jcoMaySuspend: !1 }),
			"ble-discover": Object.assign(exports0[14], { _jcoMaySuspend: !1 }),
			"ble-notifications": Object.assign(exports0[19], { _jcoMaySuspend: !1 }),
			"ble-notify": Object.assign(exports0[7], { _jcoMaySuspend: !1 }),
			"ble-read": Object.assign(exports0[20], { _jcoMaySuspend: !1 }),
			"ble-scan": Object.assign(exports0[21], { _jcoMaySuspend: !1 }),
			"ble-set-gatt": Object.assign(exports0[15], { _jcoMaySuspend: !1 }),
			"ble-stop-advertising": Object.assign(exports0[5], { _jcoMaySuspend: !1 }),
			"ble-subscribe": Object.assign(exports0[16], { _jcoMaySuspend: !1 }),
			"ble-take-writes": Object.assign(exports0[18], { _jcoMaySuspend: !1 }),
			"ble-write": Object.assign(exports0[8], { _jcoMaySuspend: !1 }),
			console: Object.assign(exports0[0], { _jcoMaySuspend: !1 }),
			"i2c-transfer": Object.assign(exports0[1], { _jcoMaySuspend: !1 }),
			"radio-configure": Object.assign(exports0[3], { _jcoMaySuspend: !1 }),
			"radio-receive": Object.assign(exports0[17], { _jcoMaySuspend: !1 }),
			"radio-send": Object.assign(exports0[12], { _jcoMaySuspend: !1 }),
			"tcp-close": Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline1, !1), { _jcoMaySuspend: !1 }),
			"tcp-connect": Object.assign(exports0[9], { _jcoMaySuspend: !1 }),
			"tcp-read": Object.assign(exports0[11], { _jcoMaySuspend: !1 }),
			"tcp-write": Object.assign(exports0[10], { _jcoMaySuspend: !1 }),
			"uart-write": Object.assign(exports0[6], { _jcoMaySuspend: !1 }),
			"wifi-join": Object.assign(exports0[22], { _jcoMaySuspend: !1 }),
			"wifi-scan": Object.assign(exports0[23], { _jcoMaySuspend: !1 })
		} }));
		memory0 = exports1.memory;
		realloc0 = (oldPtr, oldSize, align, newSize) => exports1.cabi_realloc(oldPtr, oldSize, align, newSize) >>> 0;
		try {
			WebAssembly.promising(exports1.cabi_realloc);
		} catch {}
		({exports: exports2} = instantiateCore(module2, {
			actual: {
				0: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline2, !1), { _jcoMaySuspend: !1 }),
				1: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline3, !1), { _jcoMaySuspend: !1 }),
				10: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline12, !1), { _jcoMaySuspend: !1 }),
				11: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline13, !1), { _jcoMaySuspend: !1 }),
				12: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline14, !1), { _jcoMaySuspend: !1 }),
				13: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline15, !1), { _jcoMaySuspend: !1 }),
				14: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline16, !1), { _jcoMaySuspend: !1 }),
				15: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline17, !1), { _jcoMaySuspend: !1 }),
				16: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline18, !1), { _jcoMaySuspend: !1 }),
				17: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline19, !1), { _jcoMaySuspend: !1 }),
				18: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline20, !1), { _jcoMaySuspend: !1 }),
				19: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline21, !1), { _jcoMaySuspend: !1 }),
				2: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline4, !1), { _jcoMaySuspend: !1 }),
				20: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline22, !1), { _jcoMaySuspend: !1 }),
				21: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline23, !1), { _jcoMaySuspend: !1 }),
				22: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline24, !1), { _jcoMaySuspend: !1 }),
				23: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline25, !1), { _jcoMaySuspend: !1 }),
				3: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline5, !1), { _jcoMaySuspend: !1 }),
				4: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline6, !1), { _jcoMaySuspend: !1 }),
				5: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline7, !1), { _jcoMaySuspend: !1 }),
				6: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline8, !1), { _jcoMaySuspend: !1 }),
				7: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline9, !1), { _jcoMaySuspend: !1 }),
				8: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline10, !1), { _jcoMaySuspend: !1 }),
				9: Object.assign(_guardTrap(_jcoComponentRecordTrapContext, 0, trampoline11, !1), { _jcoMaySuspend: !1 })
			},
			shim: { $imports: exports0.$imports }
		}));
		postReturn0 = exports1["cabi_post_litz:mcu/chip@0.2.0#info"];
		try {
			WebAssembly.promising(exports1["cabi_post_litz:mcu/chip@0.2.0#info"]);
		} catch {
			exports1["cabi_post_litz:mcu/chip@0.2.0#info"];
		}
		postReturn1 = exports1["cabi_post_litz:mcu/chip@0.2.0#boot"];
		try {
			WebAssembly.promising(exports1["cabi_post_litz:mcu/chip@0.2.0#boot"]);
		} catch {
			exports1["cabi_post_litz:mcu/chip@0.2.0#boot"];
		}
		postReturn2 = exports1["cabi_post_litz:mcu/chip@0.2.0#take-outputs"];
		try {
			WebAssembly.promising(exports1["cabi_post_litz:mcu/chip@0.2.0#take-outputs"]);
		} catch {
			exports1["cabi_post_litz:mcu/chip@0.2.0#take-outputs"];
		}
		postReturn3 = exports1["cabi_post_litz:mcu/chip@0.2.0#read"];
		try {
			WebAssembly.promising(exports1["cabi_post_litz:mcu/chip@0.2.0#read"]);
		} catch {
			exports1["cabi_post_litz:mcu/chip@0.2.0#read"];
		}
		chip020Info = exports1["litz:mcu/chip@0.2.0#info"];
		chip020LoadImage = exports1["litz:mcu/chip@0.2.0#load-image"];
		chip020Reset = exports1["litz:mcu/chip@0.2.0#reset"];
		chip020Boot = exports1["litz:mcu/chip@0.2.0#boot"];
		chip020SetInputs = exports1["litz:mcu/chip@0.2.0#set-inputs"];
		chip020Run = exports1["litz:mcu/chip@0.2.0#run"];
		chip020TakeOutputs = exports1["litz:mcu/chip@0.2.0#take-outputs"];
		chip020WriteConsole = exports1["litz:mcu/chip@0.2.0#write-console"];
		chip020Read = exports1["litz:mcu/chip@0.2.0#read"];
		let chip020 = {
			boot,
			info,
			loadImage,
			read,
			reset,
			run,
			setInputs,
			takeOutputs,
			writeConsole
		};
		return {
			_util: {
				stop: _stopInstance.bind(void 0, _jcoComponentStopContext),
				whenIdle: _whenIdle.bind(void 0, _jcoComponentIdleContext),
				isComponentError: (value) => value instanceof ComponentError
			},
			chip: chip020,
			"litz:mcu/chip@0.2.0": chip020
		};
	})();
	let promise, resolve, reject;
	function normalizeInstantiationError(e) {
		if (WebAssembly !== void 0 && typeof WebAssembly.SuspendError == "function" && e instanceof WebAssembly.SuspendError) return new WebAssembly.RuntimeError("cannot block a synchronous task before returning");
		return e;
	}
	function runNext(value) {
		try {
			let done;
			do
				({value, done} = gen.next(value));
			while (!(value instanceof Promise) && !done);
			if (done) {
				if (resolve) return resolve(value);
				else return value;
			}
			promise ||= new Promise((_resolve, _reject) => (resolve = _resolve, reject = _reject));
			value.then((nextVal) => done ? resolve() : runNext(nextVal), (e) => reject(normalizeInstantiationError(e)));
		} catch (e) {
			e = normalizeInstantiationError(e);
			if (reject) reject(e);
			else throw e;
		}
	}
	let maybeSyncReturn = runNext(null);
	return promise || maybeSyncReturn;
}
export const _util = {};
