#!/bin/bash

ENV_FILE=".env"

if [ ! -f $ENV_FILE ]; then
  echo "Creating $ENV_FILE file..."
  touch $ENV_FILE
else
  echo "$ENV_FILE already exists. It will be updated with new values."
fi

read -p "Enter the API key for RPC and WSS: " API_KEY
read -p "Enter the private key for throwaway WETH wallet: " PRIVATE_KEY
read -p "Enter max balance: " MAX_BALANCE

NETWORK_RPC="https://mainnet.infura.io/v3/$API_KEY"
NETWORK_WSS="wss://mainnet.infura.io/ws/v3/$API_KEY"
PRIVATE_KEY="$PRIVATE_KEY"
MAX_BALANCE="$MAX_BALANCE"

echo "Writing to $ENV_FILE..."
echo "NETWORK_RPC=$NETWORK_RPC" > $ENV_FILE
echo "NETWORK_WSS=$NETWORK_WSS" >> $ENV_FILE
echo "PRIVATE_KEY=$PRIVATE_KEY" >> $ENV_FILE
echo "MAX_BALANCE=$MAX_BALANCE" >> $ENV_FILE

echo "Done! Your .env file has been updated."
