use blockifier::{
    transaction::transaction_execution::Transaction,
    transaction::{errors::TransactionExecutionError, transactions::ExecutableTransaction},
    transaction::objects::FeeType,
};
use pathfinder_common::TransactionHash;
use primitive_types::U256;

use crate::{
    transaction::transaction_hash,
    types::{
        DeclareTransactionTrace, DeployAccountTransactionTrace, ExecuteInvocation,
        InvokeTransactionTrace, L1HandlerTransactionTrace,
    },
};

use super::{
    error::CallError,
    execution_state::ExecutionState,
    types::{FeeEstimate, TransactionSimulation, TransactionTrace},
};

pub fn simulate(
    mut execution_state: ExecutionState,
    transactions: Vec<Transaction>,
    skip_validate: bool,
    skip_fee_charge: bool,
) -> Result<Vec<TransactionSimulation>, CallError> {
    let gas_price = execution_state.gas_price;
    let block_number = execution_state.block_number;

    let (mut state, block_context) = execution_state.starknet_state()?;

    let mut simulations = Vec::with_capacity(transactions.len());
    for (transaction_idx, transaction) in transactions.into_iter().enumerate() {
        let _span = tracing::debug_span!("simulate", transaction_hash=%super::transaction::transaction_hash(&transaction), %block_number, %transaction_idx).entered();

        let transaction_type = transaction_type(&transaction);
        let fee_type = fee_type(&transaction);

        let tx_info = transaction
            .execute(&mut state, &block_context, !skip_fee_charge, !skip_validate)
            .and_then(|mut tx_info| {
                // skipping fee charge in .execute() means that the fee isn't calculated, do that explicitly
                // some other cases, like having max_fee=0 also lead to not calculating fees
                if tx_info.actual_fee.0 == 0 {
                    tx_info.actual_fee = blockifier::fee::fee_utils::calculate_tx_fee(
                        &tx_info.actual_resources,
                        &block_context,
                        // TODO: Fix this according to transaction type
                        &fee_type,
                    )?
                };
                Ok(tx_info)
            });

        match tx_info {
            Ok(tx_info) => {
                tracing::trace!(actual_fee=%tx_info.actual_fee.0, actual_resources=?tx_info.actual_resources, "Transaction simulation finished");

                simulations.push(TransactionSimulation {
                    fee_estimation: FeeEstimate {
                        gas_consumed: U256::from(tx_info.actual_fee.0) / gas_price.max(1.into()),
                        gas_price,
                        overall_fee: tx_info.actual_fee.0.into(),
                    },
                    trace: to_trace(transaction_type, tx_info)?,
                });
            }
            Err(error) => {
                tracing::debug!(%error, %transaction_idx, "Transaction simulation failed");
                return Err(error.into());
            }
        }
    }
    Ok(simulations)
}

pub fn trace_one(
    mut execution_state: ExecutionState,
    transactions: Vec<Transaction>,
    target_transaction_hash: TransactionHash,
    charge_fee: bool,
    validate: bool,
) -> Result<TransactionTrace, CallError> {
    let (mut state, block_context) = execution_state.starknet_state()?;

    for tx in transactions {
        let hash = transaction_hash(&tx);
        let tx_type = transaction_type(&tx);
        // dbg!(hash);
        let tx_info = tx.execute(&mut state, &block_context, charge_fee, validate)?;
        let trace = to_trace(tx_type, tx_info)?;
        if hash == target_transaction_hash {
            return Ok(trace);
        }
    }

    Err(CallError::Internal(anyhow::anyhow!(
        "Transaction hash not found: {}",
        target_transaction_hash
    )))
}

pub fn trace_all(
    mut execution_state: ExecutionState,
    transactions: Vec<Transaction>,
    charge_fee: bool,
    validate: bool,
) -> Result<Vec<(TransactionHash, TransactionTrace)>, CallError> {
    let (mut state, block_context) = execution_state.starknet_state()?;

    let mut ret = Vec::with_capacity(transactions.len());
    for tx in transactions {
        let hash = transaction_hash(&tx);
        let tx_type = transaction_type(&tx);
        let tx_info = tx.execute(&mut state, &block_context, charge_fee, validate)?;
        let trace = to_trace(tx_type, tx_info)?;
        ret.push((hash, trace));
    }

    Ok(ret)
}

enum TransactionType {
    Declare,
    DeployAccount,
    Invoke,
    L1Handler,
}

fn transaction_type(transaction: &Transaction) -> TransactionType {
    match transaction {
        Transaction::AccountTransaction(tx) => match tx {
            blockifier::transaction::account_transaction::AccountTransaction::Declare(_) => {
                TransactionType::Declare
            }
            blockifier::transaction::account_transaction::AccountTransaction::DeployAccount(_) => {
                TransactionType::DeployAccount
            }
            blockifier::transaction::account_transaction::AccountTransaction::Invoke(_) => {
                TransactionType::Invoke
            }
        },
        Transaction::L1HandlerTransaction(_) => TransactionType::L1Handler,
    }
}

fn fee_type(transaction: &Transaction) -> FeeType {
    match transaction {
        Transaction::AccountTransaction(_) => FeeType::Strk,
        Transaction::L1HandlerTransaction(_) => FeeType::Eth,
    }
}

fn to_trace(
    transaction_type: TransactionType,
    execution_info: blockifier::transaction::objects::TransactionExecutionInfo,
) -> Result<TransactionTrace, TransactionExecutionError> {
    tracing::trace!(?execution_info, "Transforming trace");
    // dbg!(execution_info.clone());

    let validate_invocation = execution_info
        .validate_call_info
        .map(TryInto::try_into)
        .transpose()?;
    let maybe_function_invocation = execution_info
        .execute_call_info
        .map(TryInto::try_into)
        .transpose();
    let fee_transfer_invocation = execution_info
        .fee_transfer_call_info
        .map(TryInto::try_into)
        .transpose()?;

    let trace = match transaction_type {
        TransactionType::Declare => TransactionTrace::Declare(DeclareTransactionTrace {
            validate_invocation,
            fee_transfer_invocation,
        }),
        TransactionType::DeployAccount => {
            TransactionTrace::DeployAccount(DeployAccountTransactionTrace {
                validate_invocation,
                constructor_invocation: maybe_function_invocation?,
                fee_transfer_invocation,
            })
        }
        TransactionType::Invoke => TransactionTrace::Invoke(InvokeTransactionTrace {
            validate_invocation,
            execute_invocation: ExecuteInvocation::FunctionInvocation(maybe_function_invocation?),
            fee_transfer_invocation,
        }),
        TransactionType::L1Handler => TransactionTrace::L1Handler(L1HandlerTransactionTrace {
            function_invocation: maybe_function_invocation?,
        }),
    };

    Ok(trace)
}
