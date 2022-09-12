use std::{
    any::Any,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use crate::{
    bigint,
    hint_processor::hint_processor_utils::bigint_to_usize,
    types::relocatable::{MaybeRelocatable, Relocatable},
    vm::{
        errors::vm_errors::VirtualMachineError,
        vm_core::VirtualMachine,
        vm_memory::{memory::Memory, memory_segments::MemorySegmentManager},
    },
};

use super::builtin_hint_processor_definition::HintProcessorData;
use num_bigint::BigInt;
use pyo3::{prelude::*, py_run};

#[derive(FromPyObject, Debug)]
pub enum PyMaybeRelocatable {
    Int(BigInt),
    RelocatableValue(PyRelocatable),
}

#[pyclass(name = "Relocatable")]
#[derive(Clone, Debug)]
pub struct PyRelocatable {
    index: usize,
    offset: usize,
}

#[pymethods]
impl PyRelocatable {
    #[new]
    pub fn new(tuple: (usize, usize)) -> PyRelocatable {
        PyRelocatable {
            index: tuple.0,
            offset: tuple.1,
        }
    }

    pub fn __add__(&self, value: usize) -> PyRelocatable {
        PyRelocatable {
            index: self.index,
            offset: self.offset + value,
        }
    }

    pub fn __sub__(&self, value: PyMaybeRelocatable, py: Python) -> PyResult<PyObject> {
        match value {
            PyMaybeRelocatable::Int(value) => {
                Ok(PyMaybeRelocatable::RelocatableValue(PyRelocatable {
                    index: self.index,
                    offset: self.offset - bigint_to_usize(&value).unwrap(),
                })
                .to_object(py))
            }
            PyMaybeRelocatable::RelocatableValue(address) => {
                if self.index == address.index && self.offset >= address.offset {
                    return Ok(
                        PyMaybeRelocatable::Int(bigint!(self.offset - address.offset))
                            .to_object(py),
                    );
                }
                todo!()
            }
        }
    }

    pub fn __repr__(&self) -> String {
        format!("({}, {})", self.index, self.offset)
    }
}

impl PyRelocatable {
    pub fn to_relocatable(&self) -> Relocatable {
        Relocatable {
            segment_index: self.index,
            offset: self.offset,
        }
    }
}

impl ToPyObject for PyMaybeRelocatable {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        match self {
            PyMaybeRelocatable::RelocatableValue(address) => address.clone().into_py(py),
            PyMaybeRelocatable::Int(value) => value.clone().into_py(py),
        }
    }
}

#[derive(Debug)]
pub enum MemoryOperation {
    AddSegment,
    WriteMemory(PyRelocatable, PyMaybeRelocatable),
    ReadMemory(PyRelocatable),
    End,
}

#[derive(Debug)]
pub enum MemoryResult {
    Reading(PyMaybeRelocatable),
    Segment(PyRelocatable),
    Success,
}

#[pyclass]
pub struct PySegmentManager {
    operation_sender: Sender<MemoryOperation>,
    result_receiver: Receiver<MemoryResult>,
}

#[pymethods]
impl PySegmentManager {
    pub fn add(&self) -> PyResult<PyRelocatable> {
        self.operation_sender
            .send(MemoryOperation::AddSegment)
            .unwrap();
        if let MemoryResult::Segment(result) = self.result_receiver.recv().unwrap() {
            return Ok(result);
        }
        todo!()
    }
}

impl PySegmentManager {
    pub fn new(
        operation_sender: Sender<MemoryOperation>,
        result_receiver: Receiver<MemoryResult>,
    ) -> PySegmentManager {
        PySegmentManager {
            operation_sender,
            result_receiver,
        }
    }
}

#[pyclass]
pub struct PyMemory {
    operation_sender: Sender<MemoryOperation>,
    result_receiver: Receiver<MemoryResult>,
}

#[pymethods]
impl PyMemory {
    pub fn __getitem__(&self, key: &PyRelocatable, py: Python) -> PyResult<PyObject> {
        self.operation_sender
            .send(MemoryOperation::ReadMemory(PyRelocatable::new((
                key.index, key.offset,
            ))))
            .unwrap();
        if let MemoryResult::Reading(result) = self.result_receiver.recv().unwrap() {
            return Ok(result.to_object(py));
        }
        todo!()
    }

    pub fn __setitem__(&self, key: &PyRelocatable, value: PyMaybeRelocatable) -> PyResult<()> {
        self.operation_sender
            .send(MemoryOperation::WriteMemory(
                PyRelocatable::new((key.index, key.offset)),
                value,
            ))
            .unwrap();
        self.result_receiver.recv().unwrap();
        Ok(())
    }
}

impl PyMemory {
    pub fn new(
        operation_sender: Sender<MemoryOperation>,
        result_receiver: Receiver<MemoryResult>,
    ) -> PyMemory {
        PyMemory {
            operation_sender,
            result_receiver,
        }
    }
}
fn handle_memory_messages(
    operation_receiver: Receiver<MemoryOperation>,
    result_sender: Sender<MemoryResult>,
    segment_result_sender: Sender<MemoryResult>,
    memory: &mut Memory,
    segments: &mut MemorySegmentManager,
) {
    loop {
        match operation_receiver.recv().unwrap() {
            MemoryOperation::End => break,
            MemoryOperation::ReadMemory(address) => {
                if let Some(value) = memory.get(&address.to_relocatable()).unwrap() {
                    match value {
                        MaybeRelocatable::Int(value) => result_sender
                            .send(MemoryResult::Reading(PyMaybeRelocatable::Int(
                                value.clone(),
                            )))
                            .unwrap(),
                        MaybeRelocatable::RelocatableValue(value) => result_sender
                            .send(MemoryResult::Reading(PyMaybeRelocatable::RelocatableValue(
                                PyRelocatable::new((value.segment_index, value.offset)),
                            )))
                            .unwrap(),
                    }
                };
            }
            MemoryOperation::WriteMemory(key, value) => {
                match value {
                    PyMaybeRelocatable::Int(value) => {
                        memory.insert(&key.to_relocatable(), &value).unwrap();
                    }
                    PyMaybeRelocatable::RelocatableValue(address) => {
                        memory
                            .insert(&key.to_relocatable(), &address.to_relocatable())
                            .unwrap();
                    }
                }
                result_sender.send(MemoryResult::Success).unwrap();
            }
            MemoryOperation::AddSegment => {
                let result = segments.add(memory);
                segment_result_sender
                    .send(MemoryResult::Segment(PyRelocatable::new((
                        result.segment_index,
                        result.offset,
                    ))))
                    .unwrap()
            }
        }
    }
}

pub struct PythonExecutor {}

impl PythonExecutor {
    pub fn execute_hint(
        vm: &mut VirtualMachine,
        hint_data: &Box<dyn Any>,
    ) -> Result<(), VirtualMachineError> {
        let hint_data = hint_data
            .downcast_ref::<HintProcessorData>()
            .ok_or(VirtualMachineError::WrongHintData)?;
        let code = hint_data.code.clone();

        let (operation_sender, operation_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel::<MemoryResult>();
        let (segment_result_sender, segment_result_receiver) = mpsc::channel::<MemoryResult>();
        let ap = vm.run_context.ap;
        let fp = vm.run_context.fp;
        let gil = Python::acquire_gil();
        let py = gil.python();
        py.allow_threads(move || {
            thread::spawn(move || {
                println!(" -- Starting python hint execution -- ");
                let gil = Python::acquire_gil();
                let py = gil.python();
                let memory =
                    PyCell::new(py, PyMemory::new(operation_sender.clone(), result_receiver))
                        .unwrap();
                let segments = PyCell::new(
                    py,
                    PySegmentManager::new(operation_sender.clone(), segment_result_receiver),
                )
                .unwrap();
                let ap = PyCell::new(py, PyRelocatable::new((1, ap))).unwrap();
                let fp = PyCell::new(py, PyRelocatable::new((1, fp))).unwrap();
                py_run!(py, memory segments ap fp, &code);
                println!(" -- Ending python hint -- ");
                operation_sender.send(MemoryOperation::End).unwrap();
            });
            handle_memory_messages(
                operation_receiver,
                result_sender,
                segment_result_sender,
                &mut vm.memory,
                &mut vm.segments,
            );
        });
        Ok(())
    }
}
