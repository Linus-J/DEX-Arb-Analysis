#!/bin/bash

ENV_FILE=".env.example"

if [ ! -f $ENV_FILE ]; then
  echo "Creating $ENV_FILE file..."
  touch $ENV_FILE
else
  echo "$ENV_FILE already exists. It will be updated with new values."
fi

read -p "Enter the API key for NETWORK_RPC: " RPC_API_KEY
read -p "Enter the API key for NETWORK_WSS: " WSS_API_KEY

NETWORK_RPC="https://mainnet.infura.io/v3/$RPC_API_KEY"
NETWORK_WSS="https://mainnet.infura.io/v3/$WSS_API_KEY"

echo "Writing to $ENV_FILE..."
echo "NETWORK_RPC=$NETWORK_RPC" > $ENV_FILE
echo "NETWORK_WSS=$NETWORK_WSS" >> $ENV_FILE

echo "Done! Your .env.example file has been updated."
